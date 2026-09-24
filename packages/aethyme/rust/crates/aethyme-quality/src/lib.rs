//! Optional repository-quality analysis and the legacy AI-readiness scorecard.
//!
//! `quality inspect` is the maintained, bounded surface. It scans a tracked,
//! relevant snapshot and records detector applicability. Its suggestions are
//! advisory and never feed operational readiness. `ai-ready` preserves the
//! old scorecard contract as a deprecated compatibility alias.
//!
//! The legacy implementation is a port of `src/scorecard/` (the
//! `Finding` model, eight detectors, the integer 100-point scoring
//! engine, and the json/md renderers) and `src/autofixers/` (`fix`: the
//! safety/risk engine, patch generation and application, five fixers,
//! and the git/PR helper behind `aethyme autofix`).
//!
//! # Frozen (decision D3)
//!
//! The crate is frozen: it takes fixes, not features. The byte-parity
//! contract with the retired Python implementation is withdrawn, and
//! the emulation layer that served it (CPython `json.dumps`, `str`, and
//! `difflib` ports) is gone: JSON is `serde_json`, diffs are `similar`
//! unified diffs, strings and paths are std. `ai-ready` and `autofix`
//! remain working, internal commands; `quality inspect` is the
//! maintained surface.

pub mod ai_ready_cli;
pub mod autofix_cli;
pub mod detectors;
pub mod engine;
pub mod fix;
pub mod format;
pub mod model;
pub mod quality_cli;
pub mod snapshot;
pub mod util;
pub mod walk;

#[cfg(test)]
mod testsupport;
