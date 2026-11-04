//! The integration-test binary for this crate's suites.
//!
//! Each `tests/<suite>.rs` listed below is a module of this one binary rather
//! than a binary of its own. Every separate integration-test binary links the
//! crate and its dependencies again, which made linking most of the workspace's
//! test compile time; one binary per crate links once. Tests keep their names,
//! now prefixed by their suite: run one suite with
//! `cargo test -p aethyme-contracts --test it analysis_vectors::` or
//! `cargo nextest run -p aethyme-contracts -E 'binary(it) & test(/^analysis_vectors::/)'`.
//!
//! `#[path]` keeps each suite's file, and the files it loads with its own
//! `mod` declarations, where they were: a module loaded through `#[path]`
//! resolves its child modules from its own directory, as a crate root does.
//!
//! The suites that stay separate binaries, and why, are the other `[[test]]`
//! targets in this crate's `Cargo.toml`. `aethyme-testkit/tests/test_targets.rs`
//! fails when a `tests/*.rs` file is neither listed here nor its own target,
//! because `autotests = false` would otherwise skip it silently.

#[path = "analysis_vectors.rs"]
mod analysis_vectors;
#[path = "brief_vectors.rs"]
mod brief_vectors;
#[path = "canonical_json_vectors.rs"]
mod canonical_json_vectors;
#[path = "contribution_revision.rs"]
mod contribution_revision;
#[path = "project_association_vectors.rs"]
mod project_association_vectors;
#[path = "project_identity_vectors.rs"]
mod project_identity_vectors;
#[path = "record_vectors.rs"]
mod record_vectors;
#[path = "source_snapshot_vectors.rs"]
mod source_snapshot_vectors;
#[path = "symbol_identity_vectors.rs"]
mod symbol_identity_vectors;
