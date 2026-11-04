//! The integration-test binary for this crate's suites.
//!
//! Each `tests/<suite>.rs` listed below is a module of this one binary rather
//! than a binary of its own. Every separate integration-test binary links the
//! crate and its dependencies again, which made linking most of the workspace's
//! test compile time; one binary per crate links once. Tests keep their names,
//! now prefixed by their suite: run one suite with
//! `cargo test -p aethyme-graph-indexer --test it cli_binary::` or
//! `cargo nextest run -p aethyme-graph-indexer -E 'binary(it) & test(/^cli_binary::/)'`.
//!
//! `#[path]` keeps each suite's file, and the files it loads with its own
//! `mod` declarations, where they were: a module loaded through `#[path]`
//! resolves its child modules from its own directory, as a crate root does.
//!
//! The suites that stay separate binaries, and why, are the other `[[test]]`
//! targets in this crate's `Cargo.toml`. `aethyme-testkit/tests/test_targets.rs`
//! fails when a `tests/*.rs` file is neither listed here nor its own target,
//! because `autotests = false` would otherwise skip it silently.

#[path = "cli_binary.rs"]
mod cli_binary;
#[path = "coverage.rs"]
mod coverage;
#[path = "edge_production.rs"]
mod edge_production;
#[path = "filesystem.rs"]
mod filesystem;
#[path = "link_cli.rs"]
mod link_cli;
#[path = "linker.rs"]
mod linker;
#[path = "php_indexer.rs"]
mod php_indexer;
#[path = "pipeline.rs"]
mod pipeline;
#[path = "python_indexer.rs"]
mod python_indexer;
#[path = "query_cli.rs"]
mod query_cli;
#[path = "relationships.rs"]
mod relationships;
#[path = "rust_indexer.rs"]
mod rust_indexer;
#[path = "surface_flow_indexer.rs"]
mod surface_flow_indexer;
#[path = "typescript_indexer.rs"]
mod typescript_indexer;
