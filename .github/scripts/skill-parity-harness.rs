// Run the existing std-only skill parity integration tests with rustc alone.
// The docs-only CI lane uses this shim so checking Markdown mirrors does not
// compile the Cargo workspace merely to provide aethyme_testkit::repo_root.
extern crate self as aethyme_testkit;

use std::path::PathBuf;

pub fn repo_root() -> PathBuf {
    std::env::var_os("AETHYME_TEST_REPO_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("current directory is unavailable"))
}

#[path = "../../packages/aethyme/rust/crates/aethyme-testkit/tests/skill_surface_parity.rs"]
mod skill_surface_parity;
