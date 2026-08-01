//! Autofix (fix side of the quality domain) — port of
//! `src/autofixers/`.
//!
//! Layering matches the Python package: `safety` decides what may be
//! touched and how risky it is, `patch` turns proposed content into
//! diffs and applies them, the fixers propose the content, and
//! `github` wraps the whole thing in a branch/commit/push/PR flow.
//!
//! Byte parity is the contract (retirement decision #2): produced
//! unified diffs, stdout, exit codes, and post-apply trees match the
//! Python implementation on the parity corpus. Where the Python has a
//! quirk — malformed-looking diffs from `lineterm=""`, `str.replace`
//! against a progressively-rewritten buffer, risk patterns matched
//! against absolute paths — the quirk is ported, not corrected.

pub mod pystr;
pub mod safety;
