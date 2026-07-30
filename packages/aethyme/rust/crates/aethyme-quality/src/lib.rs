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

pub mod detectors;
pub mod engine;
pub mod format;
pub mod model;
pub mod util;
pub mod walk;

#[cfg(test)]
mod testsupport;
