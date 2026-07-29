//! Deployment and templating for Aethyme discoverability
//! (python-retirement Phase 2).
//!
//! Ports `src/enhance.py` + `src/indexing/skills.py` + the deploy-facing
//! parts of `src/indexing/onboarding.py` and
//! `src/indexing/experience_telemetry.py`. Templates are EMBEDDED at
//! build time (`include_str!` from `skills/aethyme/`) so a
//! `cargo install`ed binary deploys without a source checkout — the
//! one-binary install story. The Python original read templates from
//! `AETHYME_ROOT` at runtime; embedding is the sanctioned packaging
//! change, with byte-identical rendered output verified by
//! `scripts/migration/enhance-golden.sh`.
//!
//! The router dispatches `enhance deploy|verify` here only when
//! `AETHYME_ENHANCE_NATIVE=1`; the Python side stays fully intact and
//! keeps answering by default until the flip.

pub mod agents;
pub mod deploy;
pub mod onboarding;
pub mod pyjson;
pub mod render;
pub mod skills;
pub mod telemetry;
pub mod templates;
pub mod timeutil;
pub mod util;

/// The placeholder every template substitutes.
pub const PLACEHOLDER: &str = "{{AETHYME_ROOT}}";
pub const BLOCK_BEGIN: &str = "<!-- AETHYME:BEGIN generated -->";
pub const BLOCK_END: &str = "<!-- AETHYME:END generated -->";
pub const AGENTS_OVERRIDE_PATH: &str = ".aethyme/overrides/agents.json";
