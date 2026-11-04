//! The integration-test binary for this crate's suites.
//!
//! Each `tests/<suite>.rs` listed below is a module of this one binary rather
//! than a binary of its own. Every separate integration-test binary links the
//! crate and its dependencies again, which made linking most of the workspace's
//! test compile time; one binary per crate links once. Tests keep their names,
//! now prefixed by their suite: run one suite with
//! `cargo test -p aethyme-testkit --test it build_source_identity::` or
//! `cargo nextest run -p aethyme-testkit -E 'binary(it) & test(/^build_source_identity::/)'`.
//!
//! `#[path]` keeps each suite's file, and the files it loads with its own
//! `mod` declarations, where they were: a module loaded through `#[path]`
//! resolves its child modules from its own directory, as a crate root does.
//!
//! The suites that stay separate binaries, and why, are the other `[[test]]`
//! targets in this crate's `Cargo.toml`. `aethyme-testkit/tests/test_targets.rs`
//! fails when a `tests/*.rs` file is neither listed here nor its own target,
//! because `autotests = false` would otherwise skip it silently.

#[path = "build_source_identity.rs"]
mod build_source_identity;
#[path = "composition_fixtures.rs"]
mod composition_fixtures;
#[path = "gate_validation.rs"]
mod gate_validation;
#[path = "pr_monitor_script.rs"]
mod pr_monitor_script;
#[path = "skill_surface_parity.rs"]
mod skill_surface_parity;
#[path = "test_targets.rs"]
mod test_targets;
#[path = "workspace_dependencies.rs"]
mod workspace_dependencies;
