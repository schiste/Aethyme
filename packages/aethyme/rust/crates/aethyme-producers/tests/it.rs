//! The integration-test binary for this crate's suites.
//!
//! Each `tests/<suite>.rs` listed below is a module of this one binary rather
//! than a binary of its own. Every separate integration-test binary links the
//! crate and its dependencies again, which made linking most of the workspace's
//! test compile time; one binary per crate links once. Tests keep their names,
//! now prefixed by their suite: run one suite with
//! `cargo test -p aethyme-producers --test it configs_determinism::` or
//! `cargo nextest run -p aethyme-producers -E 'binary(it) & test(/^configs_determinism::/)'`.
//!
//! `#[path]` keeps each suite's file, and the files it loads with its own
//! `mod` declarations, where they were: a module loaded through `#[path]`
//! resolves its child modules from its own directory, as a crate root does.
//!
//! The suites that stay separate binaries, and why, are the other `[[test]]`
//! targets in this crate's `Cargo.toml`. `aethyme-testkit/tests/test_targets.rs`
//! fails when a `tests/*.rs` file is neither listed here nor its own target,
//! because `autotests = false` would otherwise skip it silently.

#[path = "configs_determinism.rs"]
mod configs_determinism;
#[path = "docs_determinism.rs"]
mod docs_determinism;
#[path = "harness.rs"]
mod harness;
#[path = "packages_determinism.rs"]
mod packages_determinism;
#[path = "risks_determinism.rs"]
mod risks_determinism;
#[path = "structure_determinism.rs"]
mod structure_determinism;
