//! Dev-only test harness for the Aethyme workspace.
//!
//! This is the Rust successor to the retired pytest `tests/support/`
//! package (python-retirement Phase 7, 2026-08-06). It carries the three
//! things every implementation-blind suite needed:
//!
//! * [`bins`] — build-if-missing resolution of the product binaries
//!   (`aethyme`, `aethyme-engine-cli`, `aethyme-graph-index`), the job
//!   `tests/support/engine_binary.py` did.
//! * [`invoke`] — subprocess invocation of the router with merged
//!   stdout+stderr, cwd, and stdin, the job `tests/support/cli_invoke.py`
//!   did.
//! * [`repos`] — programmatic fixture repositories, the job
//!   `tests/support/repo_builders.py` did.
//!
//! **Placement.** It is a workspace member rather than a
//! `tests/common/mod.rs` inside one crate because more than one crate
//! needs it: `aethyme-cli` owns the CLI-surface suites, `aethyme-engine`
//! already had its own private copy of `aethyme_bin()`, and repo-hygiene
//! suites (docs, templates, grammar provenance) belong to no product
//! crate at all. A `tests/common/mod.rs` cannot be shared across crates;
//! a library crate can. `publish = false` plus dev-dependency-only
//! consumption keeps it out of `cargo install` and out of the product
//! dependency graph.
//!
//! **Implementation-blind by construction.** Nothing here links the
//! product crates. Suites drive the built binaries and assert on stdout,
//! exit codes, and on-disk artifacts — the same contract the pytest
//! suites held, so an assertion cannot accidentally start testing an
//! internal function instead of the external behaviour.

pub mod bins;
pub mod invoke;
pub mod paths;
pub mod repos;

pub use bins::{aethyme_bin, engine_bin, graph_index_bin};
pub use invoke::{Invoke, InvokeResult, invoke_aethyme};
pub use paths::{package_root, repo_root, rust_workspace_root};

pub use serde_json;
pub use tempfile;

/// A throwaway directory for one test, mirroring pytest's `tmp_path`.
pub fn tmp_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("aethyme-test-")
        .tempdir()
        .expect("create temp dir")
}
