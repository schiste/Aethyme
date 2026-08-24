//! Deployment and templating for Aethyme discoverability
//! (python-retirement Phase 2).
//!
//! Ports `src/enhance.py` + `src/indexing/skills.py` + the deploy-facing
//! parts of `src/indexing/onboarding.py` and
//! `src/indexing/experience_telemetry.py`. Templates are EMBEDDED at
//! build time (`include_str!` from `skills/aethyme/`) so a
//! `cargo install`ed binary deploys without a source checkout — the
//! one-binary install story. The Python original read templates from
//! source checkout at runtime. Deployed commands resolve the installed
//! `aethyme` binary through PATH.
//!
//! The router dispatches `enhance deploy|verify` here unconditionally
//! (Phase 2 flip, 2026-07-29); the Python `enhance` group is deleted.

pub mod agents;
pub mod cli;
pub mod deploy;
pub mod explore_summary_cli;
pub mod hygiene;
pub mod onboarding;
pub mod pyjson;
pub mod render;
pub mod repo_cli;
pub mod skills;
pub mod telemetry;
pub mod templates;
pub mod timeutil;
pub mod util;

/// Legacy placeholder rejected by deployment verification.
pub const PLACEHOLDER: &str = "{{AETHYME_ROOT}}";
pub const BLOCK_BEGIN: &str = "<!-- AETHYME:BEGIN generated -->";
pub const BLOCK_END: &str = "<!-- AETHYME:END generated -->";
pub const AGENTS_OVERRIDE_PATH: &str = ".aethyme/overrides/agents.json";
