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

/// Extract paths from `git status --porcelain` output, validating each
/// line against the porcelain XY grammar before trusting it (#43).
///
/// PATH shims can decorate git output — the Chau7 CTO shim was observed
/// printing `ok ✓` where porcelain output belongs, which a naive
/// `line[3..]` parse reports as a dirty file named `✓` (three of four
/// agents in the 2026-07-17 batch hit this independently). A status
/// line is only believed when it matches `XY<space><path>` with X and Y
/// drawn from git's status-code alphabet. Rename lines keep the target
/// path (`old -> new` ⇒ `new`).
fn parse_porcelain_paths(out: &str) -> Vec<String> {
    const CODES: &[u8] = b" MTADRCU?!";
    out.lines()
        .filter_map(|line| {
            let bytes = line.as_bytes();
            if bytes.len() < 4
                || bytes[2] != b' '
                || !CODES.contains(&bytes[0])
                || !CODES.contains(&bytes[1])
            {
                return None;
            }
            let path = &line[3..];
            Some(path.rsplit(" -> ").next().unwrap_or(path).to_string())
        })
        .collect()
}

/// Extract paths from newline-separated path listings (`git diff
/// --name-only`, `git diff-tree --name-only`, `git ls-files`), keeping
/// only lines that can actually be git-emitted paths — the same shim
/// hazard class as [`parse_porcelain_paths`] (#43). Observed in the
/// wild: a `--- Changes ---` section header decorating dirty diff
/// listings, which a naive per-line parse turns into a phantom implicit
/// lease that EVERY dirty session appears to hold — warning all session
/// pairs against each other.
///
/// Grammar: these listings never contain blank lines or diff-rendering
/// artifacts (`--- `/`+++ ` file headers, `diff --git`, `@@` hunks),
/// and git C-quotes any path containing bytes outside printable ASCII
/// (`core.quotePath` applies to path listings), so an unquoted line
/// with such bytes (the `ok ✓` decoration class) is not a path either.
/// Trade-off, same stance as #43: a repo file literally named like a
/// decoration line is dropped; with quoting disabled by hand, raw
/// non-ASCII paths are too. Both are vanishingly rare next to the
/// observed shim corruption.
fn parse_path_lines(out: &str) -> Vec<String> {
    out.lines()
        .filter(|line| {
            if line.is_empty()
                || *line == "---"
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line.starts_with("diff --git ")
                || line.starts_with("@@ ")
            {
                return false;
            }
            line.starts_with('"') || line.bytes().all(|b| (0x20..0x7f).contains(&b))
        })
        .map(str::to_string)
        .collect()
}

/// Extract paths from NUL-separated (`-z`) git output — the stronger
/// sibling of [`parse_path_lines`] for the implicit-lease surface. Real
/// paths never contain NUL or newline, so a chunk with an embedded
/// newline betrays a shim decoration glued onto a path and is dropped;
/// unlike the line grammar, an all-printable-ASCII decoration (e.g.
/// `3 files changed`) cannot slip through as a phantom path, and
/// non-ASCII paths arrive raw instead of C-quoted. Conservative by
/// design: a decoration can swallow the one path it was glued to, but
/// no decoration ever becomes a lease.
fn parse_nul_paths(out: &str) -> Vec<String> {
    out.split('\0')
        .filter(|chunk| !chunk.is_empty() && !chunk.contains('\n'))
        .map(str::to_string)
        .collect()
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
    // trim_end ONLY: porcelain status lines carry a significant leading
    // space (` M path`), and a full trim breaks the first line's XY
    // column alignment, silently dropping that entry.
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
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

    /// Resolve a ref (branch name, tag, ...) to a commit, `None` when it
    /// does not exist.
    /// Common ancestor of `a` and `b`. Re-added 2026-07-17: removed as
    /// dead code the same day #41 made it live (session_change_base
    /// derives lease baselines from merge-base(HEAD, integration)).
    pub fn merge_base(&self, a: &str, b: &str) -> Result<String, GitError> {
        Ok(run_git(&self.root, &["merge-base", a, b])?
            .trim()
            .to_string())
    }

    /// A git config value (`git config --get`), `None` when unset.
    pub fn config_get(&self, key: &str) -> Option<String> {
        run_git(&self.root, &["config", "--get", key])
            .ok()
            .filter(|value| !value.is_empty())
    }

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
    /// derived from. NUL-separated output because shim-decorated
    /// line output was observed minting leases named `""` and
    /// `"--- Changes ---"` (2026-07-17 operational data).
    pub fn changed_files(&self, base: &str) -> Result<Vec<String>, GitError> {
        let mut files =
            parse_nul_paths(&run_git(&self.root, &["diff", "--name-only", "-z", base])?);
        let untracked = run_git(
            &self.root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?;
        files.extend(parse_nul_paths(&untracked));
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
        Ok(parse_path_lines(&run_git(
            &self.root,
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-only",
                "-r",
                "--root",
                "HEAD",
            ],
        )?))
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
        Ok(parse_porcelain_paths(&run_git(
            &self.root,
            &["status", "--porcelain", "--untracked-files=all"],
        )?))
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

#[cfg(test)]
mod path_line_tests {
    use super::parse_path_lines;

    #[test]
    fn real_paths_survive_including_spaces_and_quoted_forms() {
        let out = "src/auth.py\npath with spaces.md\n\"caf\\303\\251.rs\"\nsrc/-leading-dash.rs\n";
        assert_eq!(
            parse_path_lines(out),
            vec![
                "src/auth.py",
                "path with spaces.md",
                "\"caf\\303\\251.rs\"",
                "src/-leading-dash.rs"
            ]
        );
    }

    #[test]
    fn shim_section_headers_are_not_paths() {
        // The observed decoration: a section header prepended to a dirty
        // diff listing. Parsed naively it became an implicit lease every
        // dirty session held, warning all session pairs against each other.
        let out = "--- Changes ---\nsrc/auth.py\n";
        assert_eq!(parse_path_lines(out), vec!["src/auth.py"]);
    }

    #[test]
    fn diff_rendering_artifacts_and_status_decorations_are_discarded() {
        let out =
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n---\nok \u{2713}\n\u{2713}\n\nx\n";
        assert_eq!(parse_path_lines(out), vec!["x"]);
    }
}

#[cfg(test)]
mod porcelain_tests {
    use super::parse_porcelain_paths;

    #[test]
    fn valid_lines_yield_paths() {
        let out = " M src/a.rs\n?? new.txt\nA  staged.rs\n";
        assert_eq!(
            parse_porcelain_paths(out),
            vec!["src/a.rs", "new.txt", "staged.rs"]
        );
    }

    #[test]
    fn shim_decorations_are_discarded() {
        // The Chau7 CTO shim prints decorations like these where
        // porcelain output belongs — none may survive as a "path".
        let out = "ok \u{2713}\n\u{2713}\nok\n";
        assert!(parse_porcelain_paths(out).is_empty());
    }

    #[test]
    fn rename_lines_keep_the_target() {
        let out = "R  old.rs -> new.rs\n";
        assert_eq!(parse_porcelain_paths(out), vec!["new.rs"]);
    }
}

#[cfg(test)]
mod nul_path_tests {
    use super::parse_nul_paths;

    #[test]
    fn nul_separated_paths_round_trip() {
        let out = "src/a.rs\0dir with space/b.rs\0c.rs\0";
        assert_eq!(
            parse_nul_paths(out),
            vec!["src/a.rs", "dir with space/b.rs", "c.rs"]
        );
    }

    #[test]
    fn shim_decorations_never_become_paths() {
        // Observed 2026-07-17: a PATH shim wrapped `diff --name-only`
        // output in a "--- Changes ---" header and blank lines, which the
        // old line-based parse recorded as leases. In -z output such
        // decorations carry newlines; every chunk containing one is
        // rejected rather than risking a phantom path.
        let out = "--- Changes ---\nsrc/a.rs\0src/b.rs\0src/c.rs\n\u{1f4ca} 3 files\0";
        assert_eq!(parse_nul_paths(out), vec!["src/b.rs"]);
    }

    #[test]
    fn empty_output_yields_no_paths() {
        assert!(parse_nul_paths("").is_empty());
        assert!(parse_nul_paths("\0\0").is_empty());
    }
}
