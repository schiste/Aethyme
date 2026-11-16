//! Which Git checkout a path belongs to, and whether that checkout is a broker
//! session worktree.
//!
//! Session worktrees are cleaned up when their session finishes, so an agent
//! configuration that names a server inside one breaks later. Validation uses
//! [`Checkouts::session_worktree`] to refuse such entries, and `connect` uses
//! it to find the caller's own worktree copy of a server.
//!
//! The trait is the test seam: unit tests describe checkouts directly instead
//! of building Git repositories and broker worktree roots.

use std::path::{Path, PathBuf};

/// A checkout's top level and the common Git directory every worktree of the
/// same repository shares. Both canonical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    pub root: PathBuf,
    pub common_dir: PathBuf,
}

impl Checkout {
    /// Whether `other` is a checkout of the same repository.
    pub fn same_repository(&self, other: &Checkout) -> bool {
        self.common_dir == other.common_dir
    }
}

pub trait Checkouts {
    /// The Git checkout containing `path`, if any.
    fn checkout(&self, path: &Path) -> Option<Checkout>;
    /// The broker session worktree containing `path`, if any.
    fn session_worktree(&self, path: &Path) -> Option<Checkout>;
}

/// The real host: Git discovery plus the broker's worktree-root plan.
pub struct GitCheckouts;

impl Checkouts for GitCheckouts {
    fn checkout(&self, path: &Path) -> Option<Checkout> {
        let directory = existing_directory(path)?;
        // Most server paths (interpreters, system binaries) are in no
        // checkout; answer those without spawning Git.
        if !directory.ancestors().any(|dir| dir.join(".git").exists()) {
            return None;
        }
        let repository = crate::GitRepo::discover(&directory).ok()?;
        let root = repository.root().canonicalize().ok()?;
        let common_dir = repository.git_common_dir().ok()?.canonicalize().ok()?;
        Some(Checkout { root, common_dir })
    }

    fn session_worktree(&self, path: &Path) -> Option<Checkout> {
        let checkout = self.checkout(path)?;
        let main_root = checkout.common_dir.parent()?.to_path_buf();
        if main_root == checkout.root {
            return None;
        }
        let plan = crate::verification::default_worktree_root_plan(&main_root);
        let roots = crate::broker::lifecycle::worktree_root_candidates_for(&plan);
        roots
            .iter()
            .filter_map(|(root, _)| root.canonicalize().ok())
            .any(|root| checkout.root.starts_with(&root) && checkout.root != root)
            .then_some(checkout)
    }
}

/// `path` itself when it is a directory, else its nearest existing ancestor.
pub(crate) fn existing_directory(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|candidate| candidate.is_dir())
        .and_then(|dir| dir.canonicalize().ok())
}

/// `path` canonicalized when it exists, else its nearest existing ancestor
/// canonicalized with the rest appended, so comparisons survive symlinked
/// prefixes such as macOS `/var` -> `/private/var`.
pub(crate) fn canonical_or_lexical(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    let mut suffix = Vec::new();
    let mut current = path;
    while let Some(parent) = current.parent() {
        if let Some(name) = current.file_name() {
            suffix.push(name.to_os_string());
        }
        if let Ok(canonical) = parent.canonicalize() {
            let mut result = canonical;
            for name in suffix.iter().rev() {
                result.push(name);
            }
            return result;
        }
        current = parent;
    }
    path.to_path_buf()
}
