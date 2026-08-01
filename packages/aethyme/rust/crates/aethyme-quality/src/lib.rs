//! AI-readiness scorecard, detect side (retirement plan Phase 4).
//!
//! Port of `src/scorecard/` — the `Finding` model, the eight detectors,
//! the integer 100-point scoring engine, and the json/md report
//! renderers. The contract is byte parity with the Python
//! implementation (decision #2): identical finding sets per detector
//! (file, line, severity, message, evidence, suggestion), identical
//! scores, and `--format json`/`--format md` outputs byte-identical
//! after volatile-field normalization (scan_id, timestamps, durations).
//!
//! Parity-first discipline: detectors replicate Python quirks on
//! purpose (see per-detector comments); improvements — graph-backed
//! detection, smarter heuristics — are V2 material, not this port.
//!
//! # Fix side (retirement plan Phase 5)
//!
//! `fix` is the port of `src/autofixers/`: the safety/risk engine,
//! patch generation and application, the five fixers, and the git/PR
//! helper, behind the native `aethyme autofix` front end. Same
//! contract, extended to the produced unified diffs: byte-identical
//! patches on the parity corpus.
//!
//! The two sides share the crate (decision #1: "the unification is the
//! point") but not yet a scan. The fixers keep their own scanning
//! rather than consuming `Finding`s, because the two disagree about
//! what counts — see `fix::fixers` for why that unification is deferred
//! to a post-parity refactor.

pub mod ai_ready_cli;
pub mod autofix_cli;
pub mod detectors;
pub mod engine;
pub mod fix;
pub mod format;
pub mod model;
pub mod util;
pub mod walk;

#[cfg(test)]
mod testsupport;
