//! Git service layer: every git operation the broker performs, in one
//! module, shelling out to the `git` binary (no libgit2/gix until a
//! measured need appears — see docs/aethyme-local-agent-broker.md).
//!
//! No other broker code may run git directly. Keeping the surface here
//! makes the broker's git contract auditable and testable against real
//! throwaway repositories.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Errors from git operations. `Git` carries the failing subcommand and
/// stderr so callers can surface actionable messages verbatim.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("failed to run git {args}: {source}")]
    Spawn {
        args: String,
        source: std::io::Error,
    },

    #[error("git {args} failed: {stderr}")]
    Git { args: String, stderr: String },

    #[error("{path} is not inside a git repository")]
    NotARepository { path: PathBuf },
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|source| GitError::Spawn {
            args: args.join(" "),
            source,
        })?;
    if !output.status.success() {
        return Err(GitError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// A handle on one git checkout (the main repository or a linked
/// worktree). Constructed via [`GitRepo::discover`].
pub struct GitRepo {
    /// Top level of *this* checkout.
    root: PathBuf,
}

impl GitRepo {
    /// Discover the repository containing `path`.
    pub fn discover(path: &Path) -> Result<Self, GitError> {
        let root = run_git(path, &["rev-parse", "--show-toplevel"]).map_err(|_| {
            GitError::NotARepository {
                path: path.to_path_buf(),
            }
        })?;
        Ok(Self {
            root: PathBuf::from(root),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Root of the **main** checkout, even when `self` is a linked
    /// worktree. Broker state (`.aethyme/broker.db`) always lives here so
    /// all worktrees coordinate through one database.
    pub fn main_root(&self) -> Result<PathBuf, GitError> {
        let common = run_git(&self.root, &["rev-parse", "--git-common-dir"])?;
        let mut common_dir = PathBuf::from(common);
        if common_dir.is_relative() {
            common_dir = self.root.join(common_dir);
        }
        // `<main>/.git` → `<main>`. Canonicalize to strip the `..`
        // segments git emits from linked worktrees.
        let common_dir = common_dir.canonicalize().unwrap_or(common_dir);
        Ok(common_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(common_dir))
    }

    pub fn current_branch(&self) -> Result<String, GitError> {
        run_git(&self.root, &["rev-parse", "--abbrev-ref", "HEAD"])
    }

    pub fn head_commit(&self) -> Result<String, GitError> {
        run_git(&self.root, &["rev-parse", "HEAD"])
    }

    pub fn merge_base(&self, a: &str, b: &str) -> Result<String, GitError> {
        run_git(&self.root, &["merge-base", a, b])
    }

    /// Tracked-file changes (committed + staged + unstaged) against
    /// `base`, plus untracked files: the diff surface implicit leases are
    /// derived from.
    pub fn changed_files(&self, base: &str) -> Result<Vec<String>, GitError> {
        let mut files: Vec<String> = run_git(&self.root, &["diff", "--name-only", base])?
            .lines()
            .map(str::to_string)
            .collect();
        let untracked = run_git(&self.root, &["ls-files", "--others", "--exclude-standard"])?;
        files.extend(untracked.lines().map(str::to_string));
        files.sort();
        files.dedup();
        Ok(files)
    }

    /// True when the checkout has uncommitted changes or untracked files —
    /// the guard `broker cleanup` consults before removing a worktree.
    pub fn is_dirty(&self) -> Result<bool, GitError> {
        Ok(!run_git(
            &self.root,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .is_empty())
    }

    /// Commits on HEAD that are not reachable from `base` — cleanup
    /// refuses to remove a worktree whose work was never merged.
    pub fn unmerged_commit_count(&self, base: &str) -> Result<u64, GitError> {
        let count = run_git(
            &self.root,
            &["rev-list", "--count", &format!("{base}..HEAD")],
        )?;
        Ok(count.parse().unwrap_or(0))
    }

    /// Create a linked worktree at `dest` on new branch `branch` starting
    /// from `base`, returning a handle on it.
    pub fn worktree_add(&self, dest: &Path, branch: &str, base: &str) -> Result<GitRepo, GitError> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|source| GitError::Spawn {
                args: format!("create_dir_all {}", parent.display()),
                source,
            })?;
        }
        run_git(
            &self.root,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                dest.to_str().unwrap_or_default(),
                base,
            ],
        )?;
        GitRepo::discover(dest)
    }

    /// Remove a linked worktree. `force` discards uncommitted changes —
    /// callers must have applied the dirty-check policy first.
    pub fn worktree_remove(&self, worktree: &Path, force: bool) -> Result<(), GitError> {
        let path = worktree.to_str().unwrap_or_default();
        let args: Vec<&str> = if force {
            vec!["worktree", "remove", "--force", path]
        } else {
            vec!["worktree", "remove", path]
        };
        run_git(&self.root, &args)?;
        Ok(())
    }

    /// Paths of all linked worktrees registered on this repository
    /// (excluding the main checkout).
    pub fn worktree_paths(&self) -> Result<Vec<PathBuf>, GitError> {
        let listing = run_git(&self.root, &["worktree", "list", "--porcelain"])?;
        let main = self.main_root()?;
        Ok(listing
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(PathBuf::from)
            .filter(|path| path.canonicalize().map(|p| p != main).unwrap_or(true))
            .collect())
    }
}
