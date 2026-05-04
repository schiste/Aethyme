//! Repository-context generation: prompt assembly and Chau7 snippet emission.
//!
//! These modules consume the in-memory `RepositoryMap`, not the persisted
//! graph store. They lived under `store::` historically because that's where
//! the SurrealDB integration sat; with redb landing as a backend-only concern,
//! they belong in their own namespace.

pub mod prompt;
pub mod snippets;
