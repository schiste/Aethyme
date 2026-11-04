//! The integration-test binary for this crate's suites.
//!
//! Each `tests/<suite>.rs` listed below is a module of this one binary rather
//! than a binary of its own. Every separate integration-test binary links the
//! crate and its dependencies again, which made linking most of the workspace's
//! test compile time; one binary per crate links once. Tests keep their names,
//! now prefixed by their suite: run one suite with
//! `cargo test -p aethyme-graph-schema --test it attributes::` or
//! `cargo nextest run -p aethyme-graph-schema -E 'binary(it) & test(/^attributes::/)'`.
//!
//! `#[path]` keeps each suite's file, and the files it loads with its own
//! `mod` declarations, where they were: a module loaded through `#[path]`
//! resolves its child modules from its own directory, as a crate root does.
//!
//! The suites that stay separate binaries, and why, are the other `[[test]]`
//! targets in this crate's `Cargo.toml`. `aethyme-testkit/tests/test_targets.rs`
//! fails when a `tests/*.rs` file is neither listed here nor its own target,
//! because `autotests = false` would otherwise skip it silently.

#[path = "attributes.rs"]
mod attributes;
#[path = "callables.rs"]
mod callables;
#[path = "common.rs"]
mod common;
#[path = "containers.rs"]
mod containers;
#[path = "cross_enum.rs"]
mod cross_enum;
#[path = "determinism.rs"]
mod determinism;
#[path = "edge_struct.rs"]
mod edge_struct;
#[path = "edges.rs"]
mod edges;
#[path = "identity.rs"]
mod identity;
#[path = "kinds.rs"]
mod kinds;
#[path = "node_enum.rs"]
mod node_enum;
#[path = "non_code.rs"]
mod non_code;
#[path = "sub_symbols.rs"]
mod sub_symbols;
#[path = "types.rs"]
mod types;
