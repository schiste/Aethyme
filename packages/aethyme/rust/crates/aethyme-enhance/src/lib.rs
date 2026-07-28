//! Deployment and templating for Aethyme discoverability
//! (python-retirement Phase 2).
//!
//! Ports `src/enhance.py` + `src/indexing/skills.py`. Templates are
//! EMBEDDED at build time (`include_str!` from `skills/aethyme/`) so a
//! `cargo install`ed binary deploys without a source checkout — the
//! one-binary install story. The Python original read templates from
//! `AETHYME_ROOT` at runtime; embedding is the sanctioned packaging
//! change, with byte-identical rendered output verified by
//! `scripts/migration/enhance-golden.sh`.
//!
//! Port status (session 1): substitution + generated-block splicing
//! primitives with tests. The deploy pipeline, override rendering,
//! settings merge, and verify land in subsequent sessions; the router
//! keeps delegating `enhance` to Python until the full pipeline flips.

pub mod render;

/// The placeholder every template substitutes.
pub const PLACEHOLDER: &str = "{{AETHYME_ROOT}}";
pub const BLOCK_BEGIN: &str = "<!-- AETHYME:BEGIN generated -->";
pub const BLOCK_END: &str = "<!-- AETHYME:END generated -->";
