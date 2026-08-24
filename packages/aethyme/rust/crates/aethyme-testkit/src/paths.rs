//! Checkout-relative paths, resolved from the runtime checkout.
//!
//! Broker gates share one Cargo target directory across temporary merge
//! worktrees. Therefore `env!("CARGO_MANIFEST_DIR")` may name the worktree
//! that first compiled a reused test binary, not the checkout currently under
//! test — and that old worktree may already be deleted. Prefer discovering the
//! repository from the process cwd and retain the compiled path only as a
//! fallback for direct test-binary invocation.

use std::path::{Path, PathBuf};

fn find_repo_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|candidate| {
        candidate
            .join("packages/aethyme/rust/Cargo.toml")
            .is_file()
            .then(|| candidate.to_path_buf())
    })
}

fn compiled_repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(5)
        .expect("checkout layout: ran out of ancestors")
        .to_path_buf()
}

/// `packages/aethyme/rust` — the Cargo workspace root.
pub fn rust_workspace_root() -> PathBuf {
    repo_root().join("packages/aethyme/rust")
}

/// `packages/aethyme` — the package root (`AETHYME_ROOT`).
pub fn package_root() -> PathBuf {
    repo_root().join("packages/aethyme")
}

/// The monorepo root, which owns `.github/` and the top-level docs.
pub fn repo_root() -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| find_repo_root(&cwd))
        .unwrap_or_else(compiled_repo_root)
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

    #[test]
    fn runtime_discovery_finds_the_checkout_from_nested_directories() {
        let compiled = compiled_repo_root();
        assert_eq!(
            find_repo_root(&compiled.join("packages/aethyme/rust/crates/aethyme-testkit/src")),
            Some(compiled)
        );
    }
}
