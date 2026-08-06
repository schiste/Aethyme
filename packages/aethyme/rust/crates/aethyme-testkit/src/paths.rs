//! Checkout-relative paths, resolved from this crate's manifest dir.
//!
//! Tests run with an arbitrary cwd (cargo sets it to the *package* root,
//! and suites routinely `cd` into temp repos), so every path here is
//! derived from `CARGO_MANIFEST_DIR` rather than from cwd. This is the
//! Rust equivalent of the pytest suites' `Path(__file__).parents[n]`
//! idiom.

use std::path::{Path, PathBuf};

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn ancestor(levels: usize) -> PathBuf {
    let mut path = manifest_dir().to_path_buf();
    for _ in 0..levels {
        path = path
            .parent()
            .expect("checkout layout: ran out of ancestors")
            .to_path_buf();
    }
    path
}

/// `packages/aethyme/rust` — the Cargo workspace root.
pub fn rust_workspace_root() -> PathBuf {
    ancestor(2)
}

/// `packages/aethyme` — the package root (`AETHYME_ROOT`).
pub fn package_root() -> PathBuf {
    ancestor(3)
}

/// The monorepo root, which owns `.github/` and the top-level docs.
pub fn repo_root() -> PathBuf {
    ancestor(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_point_at_the_expected_landmarks() {
        assert!(rust_workspace_root().join("Cargo.toml").is_file());
        assert!(package_root().join("skills/aethyme/SKILL.md").is_file());
        assert!(repo_root().join(".github/workflows").is_dir());
    }
}
