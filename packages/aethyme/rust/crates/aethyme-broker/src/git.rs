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
    run_git_inner(cwd, None, args)
}

/// Like [`run_git`] but against a private index file (GIT_INDEX_FILE) so
/// staging operations never disturb the checkout's real index.
fn run_git_with_index(cwd: &Path, index_file: &str, args: &[&str]) -> Result<String, GitError> {
    run_git_inner(cwd, Some(index_file), args)
}

fn run_git_inner(cwd: &Path, index_file: Option<&str>, args: &[&str]) -> Result<String, GitError> {
    let mut command = Command::new("git");
    command.args(args).current_dir(cwd);
    if let Some(index) = index_file {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = command.output().map_err(|source| GitError::Spawn {
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

/// Result of a `git merge-tree --write-tree` simulation.
#[derive(Debug)]
pub struct MergeSimulation {
    /// The merged tree oid (written even when conflicted).
    pub tree: String,
    /// Conflicted paths; empty means the merge is clean.
    pub conflicts: Vec<String>,
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

    /// Absolute path of the repository's **common** git dir — the one
    /// directory every linked worktree shares. Installed git hooks live
    /// in `<common>/hooks`, which is what makes them worktree-wide.
    pub fn git_common_dir(&self) -> Result<PathBuf, GitError> {
        let common = run_git(&self.root, &["rev-parse", "--git-common-dir"])?;
        let mut common_dir = PathBuf::from(common);
        if common_dir.is_relative() {
            common_dir = self.root.join(common_dir);
        }
        // Canonicalize to strip the `..` segments git emits from linked
        // worktrees.
        Ok(common_dir.canonicalize().unwrap_or(common_dir))
    }

    /// Root of the **main** checkout, even when `self` is a linked
    /// worktree. Broker state (`.aethyme/broker.db`) always lives here so
    /// all worktrees coordinate through one database.
    pub fn main_root(&self) -> Result<PathBuf, GitError> {
        // `<main>/.git` → `<main>`.
        let common_dir = self.git_common_dir()?;
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

    /// Resolve a ref (branch name, tag, ...) to a commit, `None` when it
    /// does not exist.
    pub fn resolve_ref(&self, name: &str) -> Option<String> {
        run_git(
            &self.root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{name}^{{commit}}"),
            ],
        )
        .ok()
    }

    /// True when `ancestor` is reachable from `descendant`
    /// (`git merge-base --is-ancestor`).
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        Command::new("git")
            .args(["merge-base", "--is-ancestor", ancestor, descendant])
            .current_dir(&self.root)
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    /// Create or fast-move a local branch ref to `commit` (no checkout).
    pub fn update_branch_ref(&self, branch: &str, commit: &str) -> Result<(), GitError> {
        run_git(
            &self.root,
            &["update-ref", &format!("refs/heads/{branch}"), commit],
        )?;
        Ok(())
    }

    /// Simulate merging `head` onto `base` without touching any worktree
    /// (`git merge-tree --write-tree`). Returns the resulting tree and
    /// the conflicted file list (empty = clean).
    pub fn merge_tree_simulate(&self, base: &str, head: &str) -> Result<MergeSimulation, GitError> {
        // Exit code 1 = conflicts (still writes a tree); >1 = real error.
        let output = Command::new("git")
            .args([
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                base,
                head,
            ])
            .current_dir(&self.root)
            .output()
            .map_err(|source| GitError::Spawn {
                args: "merge-tree --write-tree".into(),
                source,
            })?;
        let code = output.status.code().unwrap_or(-1);
        if code != 0 && code != 1 {
            return Err(GitError::Git {
                args: "merge-tree --write-tree".into(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines();
        let tree = lines.next().unwrap_or_default().trim().to_string();
        let conflicts: Vec<String> = lines
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        Ok(MergeSimulation { tree, conflicts })
    }

    /// Create a commit object for `tree` with `parents` (no checkout,
    /// no ref update). Used to materialize simulated merges.
    pub fn commit_tree(
        &self,
        tree: &str,
        parents: &[&str],
        message: &str,
    ) -> Result<String, GitError> {
        let mut args: Vec<String> = vec!["commit-tree".into(), tree.into()];
        for parent in parents {
            args.push("-p".into());
            args.push((*parent).into());
        }
        args.push("-m".into());
        args.push(message.into());
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = Command::new("git")
            .args(&arg_refs)
            .current_dir(&self.root)
            .env("GIT_AUTHOR_NAME", "aethyme-broker")
            .env("GIT_AUTHOR_EMAIL", "broker@aethyme.local")
            .env("GIT_COMMITTER_NAME", "aethyme-broker")
            .env("GIT_COMMITTER_EMAIL", "broker@aethyme.local")
            .output()
            .map_err(|source| GitError::Spawn {
                args: "commit-tree".into(),
                source,
            })?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: "commit-tree".into(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Changed files between two commits (for affected-gate selection on
    /// a simulated merged tree).
    pub fn changed_between(&self, from: &str, to: &str) -> Result<Vec<String>, GitError> {
        Ok(run_git(&self.root, &["diff", "--name-only", from, to])?
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// Create a detached worktree at `dest` checked out at `commit`.
    pub fn worktree_add_detached(&self, dest: &Path, commit: &str) -> Result<GitRepo, GitError> {
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
                "--detach",
                dest.to_str().unwrap_or_default(),
                commit,
            ],
        )?;
        GitRepo::discover(dest)
    }

    /// Deterministic tree hash of the checkout's current *working state*
    /// (HEAD + staged + unstaged + untracked, .gitignore respected) — the
    /// gate-result cache key. Uses a throwaway index so the real index is
    /// never touched; identical content in different worktrees hashes
    /// identically, which is what makes cross-agent gate dedup work.
    pub fn working_tree_hash(&self) -> Result<String, GitError> {
        let tmp = self.root.join(".git-broker-index.tmp");
        let tmp_str = tmp.to_string_lossy().into_owned();
        let result = (|| {
            run_git_with_index(&self.root, &tmp_str, &["read-tree", "HEAD"])?;
            run_git_with_index(&self.root, &tmp_str, &["add", "-A", "."])?;
            run_git_with_index(&self.root, &tmp_str, &["write-tree"])
        })();
        let _ = std::fs::remove_file(&tmp);
        result
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

    /// Paths staged for the next commit (`git diff --cached --name-only`)
    /// — the selection surface of the derived pre-commit hook. Inherits
    /// GIT_INDEX_FILE when git set one, so partial commits see exactly
    /// what will be committed.
    pub fn staged_files(&self) -> Result<Vec<String>, GitError> {
        Ok(run_git(&self.root, &["diff", "--cached", "--name-only"])?
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// Files changed by the HEAD commit — what the post-commit conflict
    /// radar compares against other sessions' leases. `--root` makes the
    /// initial commit report its files too.
    pub fn head_changed_files(&self) -> Result<Vec<String>, GitError> {
        Ok(run_git(
            &self.root,
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "--root",
                "HEAD",
            ],
        )?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
    }

    /// One-line summaries (short-sha + subject) of `from..to`, newest
    /// first, capped at `limit` — the submit preflight's "exactly what
    /// will be submitted" listing.
    pub fn commit_summaries(
        &self,
        from: &str,
        to: &str,
        limit: usize,
    ) -> Result<Vec<String>, GitError> {
        let range = format!("{from}..{to}");
        let n = format!("--max-count={limit}");
        let out = run_git(
            &self.root,
            &["log", "--oneline", "--no-decorate", &n, &range],
        )?;
        Ok(out.lines().map(str::to_string).collect())
    }

    /// Paths with uncommitted or untracked changes — the submit preflight
    /// warns about these because only committed work integrates.
    pub fn dirty_paths(&self) -> Result<Vec<String>, GitError> {
        Ok(run_git(
            &self.root,
            &["status", "--porcelain", "--untracked-files=all"],
        )?
        .lines()
        .filter_map(|l| l.get(3..).map(str::to_string))
        .collect())
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
