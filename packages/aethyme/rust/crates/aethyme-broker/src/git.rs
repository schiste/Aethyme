//! Git service layer: every git operation the broker performs, in one
//! module, shelling out to the `git` binary (no libgit2/gix until a
//! measured need appears — see docs/aethyme-local-agent-broker.md).
//!
//! No other broker code may run git directly. Keeping the surface here
//! makes the broker's git contract auditable and testable against real
//! throwaway repositories.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_PRIVATE_INDEX_ID: AtomicU64 = AtomicU64::new(1);

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
    parse_porcelain_entries(out)
        .into_iter()
        .map(|(_, path)| path)
        .collect()
}

fn parse_porcelain_entries(out: &str) -> Vec<([u8; 2], String)> {
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
            Some((
                [bytes[0], bytes[1]],
                path.rsplit(" -> ").next().unwrap_or(path).to_string(),
            ))
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

/// Default branch advertised by a configured Git remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDefaultBranch {
    pub ref_name: String,
    pub sha: String,
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

    /// Configured tracking ref and its currently fetched commit. This is
    /// read-only: status and reconciliation never perform an implicit fetch.
    pub fn tracking_upstream(&self) -> Option<(String, String)> {
        let upstream = run_git(
            &self.root,
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
        )
        .ok()?;
        let commit = self.resolve_ref(&upstream)?;
        Some((upstream, commit))
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

    /// Configured URL for `remote`.
    pub fn remote_url(&self, remote: &str) -> Result<String, GitError> {
        run_git(&self.root, &["remote", "get-url", remote])
    }

    /// Configured push URL for `remote`, after Git's URL rewrite rules.
    pub fn remote_push_url(&self, remote: &str) -> Result<String, GitError> {
        run_git(&self.root, &["remote", "get-url", "--push", remote])
    }

    /// Every configured push URL for `remote`, after Git's URL rewrite rules.
    ///
    /// A remote may have several `pushurl` entries. Callers coordinating a
    /// publication must inspect the complete set rather than inheriting Git's
    /// multi-destination behavior accidentally.
    pub fn remote_push_urls(&self, remote: &str) -> Result<Vec<String>, GitError> {
        Ok(run_git(
            &self.root,
            &["remote", "get-url", "--push", "--all", remote],
        )?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
    }

    /// Configured remote names in Git's deterministic display order.
    pub fn remotes(&self) -> Result<Vec<String>, GitError> {
        Ok(run_git(&self.root, &["remote"])?
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Resolve a configured remote to one credential-free coordination target.
    pub fn resolve_remote_target(
        &self,
        remote: &str,
        caller_assertion: Option<&str>,
    ) -> Result<crate::ResolvedRemoteTarget, crate::RemoteTargetError> {
        crate::remote_target::resolve_remote_target(self, remote, caller_assertion)
    }

    /// Resolve the one configured remote targeted by a Git command.
    pub fn resolve_remote_command_target(
        &self,
        args: &[String],
        caller_assertion: Option<&str>,
    ) -> Result<crate::ResolvedRemoteTarget, crate::RemoteTargetError> {
        crate::remote_target::resolve_remote_command_target(self, args, caller_assertion)
    }

    /// Resolve one explicit push source expression to the exact object ID Git
    /// would advertise to the remote. The object is not peeled: annotated tag
    /// refs must compare against their tag object, not the tagged commit.
    pub fn resolve_push_source(&self, source: &str) -> Result<String, GitError> {
        run_git(
            &self.root,
            &["rev-parse", "--verify", &format!("{source}^{{object}}")],
        )
    }

    /// Validate one fully-qualified destination ref with Git's own grammar.
    pub fn validate_push_destination(&self, destination: &str) -> Result<(), GitError> {
        run_git(&self.root, &["check-ref-format", destination]).map(|_| ())
    }

    /// Observe exact remote refs without updating local tracking refs.
    ///
    /// A successful query that omits a requested ref proves that ref is
    /// absent. Any command, parse, duplicate, or unexpected-ref failure makes
    /// the complete observation unavailable; callers must not classify from a
    /// partial parse.
    pub fn remote_ref_oids(
        &self,
        remote: &str,
        destinations: &[String],
    ) -> Result<BTreeMap<String, Option<String>>, GitError> {
        let push_url = run_git(&self.root, &["remote", "get-url", "--push", remote])?;
        let display_args = format!("ls-remote --refs <push-url> {}", destinations.join(" "));
        let output = Command::new("git")
            .args(["ls-remote", "--refs", "aethyme-push-evidence"])
            .args(destinations)
            .current_dir(&self.root)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "remote.aethyme-push-evidence.url")
            .env("GIT_CONFIG_VALUE_0", push_url)
            .output()
            .map_err(|source| GitError::Spawn {
                args: display_args.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: display_args,
                stderr: "remote ref evidence command failed".into(),
            });
        }
        let output = String::from_utf8_lossy(&output.stdout);
        let mut observed = destinations
            .iter()
            .cloned()
            .map(|destination| (destination, None))
            .collect::<BTreeMap<_, _>>();
        for line in output.lines() {
            let Some((sha, destination)) = line.split_once('\t') else {
                return Err(GitError::Git {
                    args: display_args.clone(),
                    stderr: "remote ref evidence was not a SHA/ref pair".into(),
                });
            };
            if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(GitError::Git {
                    args: display_args.clone(),
                    stderr: "remote ref evidence contained an invalid object ID".into(),
                });
            }
            let Some(slot) = observed.get_mut(destination) else {
                return Err(GitError::Git {
                    args: display_args.clone(),
                    stderr: "remote ref evidence contained an unrequested destination".into(),
                });
            };
            if slot.replace(sha.to_ascii_lowercase()).is_some() {
                return Err(GitError::Git {
                    args: display_args.clone(),
                    stderr: "remote ref evidence repeated one destination".into(),
                });
            }
        }
        Ok(observed)
    }

    /// Query the remote's advertised HEAD without updating any local ref.
    pub fn remote_default_branch(&self, remote: &str) -> Result<RemoteDefaultBranch, GitError> {
        let output = run_git(&self.root, &["ls-remote", "--symref", remote, "HEAD"])?;
        let ref_name = output
            .lines()
            .find_map(|line| {
                line.strip_prefix("ref: ")
                    .and_then(|rest| rest.split_once('\t'))
                    .filter(|(_, name)| *name == "HEAD")
                    .map(|(name, _)| name.to_string())
            })
            .ok_or_else(|| GitError::Git {
                args: format!("ls-remote --symref {remote} HEAD"),
                stderr: format!("remote {remote:?} did not advertise a symbolic HEAD"),
            })?;
        let sha = output
            .lines()
            .find_map(|line| {
                line.split_once('\t')
                    .filter(|(sha, name)| {
                        *name == "HEAD"
                            && sha.len() == 40
                            && sha.chars().all(|character| character.is_ascii_hexdigit())
                    })
                    .map(|(sha, _)| sha.to_string())
            })
            .ok_or_else(|| GitError::Git {
                args: format!("ls-remote --symref {remote} HEAD"),
                stderr: format!("remote {remote:?} did not advertise a HEAD commit"),
            })?;
        Ok(RemoteDefaultBranch { ref_name, sha })
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

    /// Human-readable description of `rev` using the repository's tags
    /// when available, falling back to an abbreviated commit.
    pub fn describe_ref(&self, rev: &str) -> Option<String> {
        run_git(&self.root, &["describe", "--tags", "--always", rev])
            .ok()
            .filter(|value| !value.is_empty())
    }

    /// Nearest reachable release tag for `rev`, if the repository has
    /// one. Used only for operator-facing version drift classification.
    pub fn nearest_tag(&self, rev: &str) -> Option<String> {
        run_git(&self.root, &["describe", "--tags", "--abbrev=0", rev])
            .ok()
            .filter(|value| !value.is_empty())
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

    /// Fast-forward the checked-out branch and worktree to an exact commit.
    /// The caller is responsible for clean-worktree and ancestry preflight;
    /// `--ff-only` keeps Git as the final enforcement boundary.
    pub fn fast_forward_checkout(&self, commit: &str) -> Result<(), GitError> {
        run_git(&self.root, &["merge", "--ff-only", commit])?;
        Ok(())
    }

    /// Create or fast-move a local branch ref to `commit` (no checkout).
    pub fn update_branch_ref(&self, branch: &str, commit: &str) -> Result<(), GitError> {
        run_git(
            &self.root,
            &["update-ref", &format!("refs/heads/{branch}"), commit],
        )?;
        Ok(())
    }

    /// Compare-and-swap a branch ref. Reconciliation uses the expected
    /// old value so a concurrent promotion cannot be silently overwritten.
    pub fn update_branch_ref_checked(
        &self,
        branch: &str,
        commit: &str,
        expected_old: &str,
    ) -> Result<(), GitError> {
        run_git(
            &self.root,
            &[
                "update-ref",
                &format!("refs/heads/{branch}"),
                commit,
                expected_old,
            ],
        )?;
        Ok(())
    }

    /// First parent of a commit, used to recover the exact promoted delta
    /// even though promotion details intentionally keep a compact payload.
    pub fn first_parent(&self, commit: &str) -> Result<String, GitError> {
        run_git(&self.root, &["rev-parse", &format!("{commit}^1")])
    }

    /// Commits reachable from `to` but not `from`, oldest first.
    pub fn commits_between_oldest(&self, from: &str, to: &str) -> Result<Vec<String>, GitError> {
        let range = format!("{from}..{to}");
        Ok(run_git(&self.root, &["rev-list", "--reverse", &range])?
            .lines()
            .map(str::to_string)
            .collect())
    }

    /// Commits reachable from `head` but not `excluded`, oldest first.
    /// Unlike a `from..to` range this remains meaningful when the two
    /// histories have diverged.
    pub fn commits_excluding_oldest(
        &self,
        head: &str,
        excluded: &str,
    ) -> Result<Vec<String>, GitError> {
        Ok(run_git(
            &self.root,
            &["rev-list", "--reverse", head, "--not", excluded],
        )?
        .lines()
        .map(str::to_string)
        .collect())
    }

    /// First-parent commits reachable from `head` but not `excluded`,
    /// oldest first. Submission provenance uses only this layer so a
    /// broker promotion merge and its submitted second parent do not
    /// appear as two patch-identity candidates.
    pub fn first_parent_commits_excluding_oldest(
        &self,
        head: &str,
        excluded: &str,
    ) -> Result<Vec<String>, GitError> {
        Ok(run_git(
            &self.root,
            &[
                "rev-list",
                "--first-parent",
                "--reverse",
                head,
                "--not",
                excluded,
            ],
        )?
        .lines()
        .map(str::to_string)
        .collect())
    }

    /// Every parent of a commit, in Git's stored order.
    pub fn commit_parents(&self, commit: &str) -> Result<Vec<String>, GitError> {
        let line = run_git(&self.root, &["rev-list", "--parents", "-n", "1", commit])?;
        Ok(line
            .split_whitespace()
            .skip(1)
            .map(str::to_string)
            .collect())
    }

    /// Tree object referenced by a commit.
    pub fn commit_tree_id(&self, commit: &str) -> Result<String, GitError> {
        run_git(&self.root, &["rev-parse", &format!("{commit}^{{tree}}")])
    }

    /// First-parent commits reachable from `to` but not `from`, oldest
    /// first. Integration reconciliation uses this to inspect only the
    /// broker-created layer, excluding submitted heads attached as second
    /// parents of promotion merges.
    pub fn first_parent_commits_between_oldest(
        &self,
        from: &str,
        to: &str,
    ) -> Result<Vec<String>, GitError> {
        let range = format!("{from}..{to}");
        Ok(run_git(
            &self.root,
            &["rev-list", "--first-parent", "--reverse", &range],
        )?
        .lines()
        .map(str::to_string)
        .collect())
    }

    /// Stable patch id for the cumulative diff `from..to`. Empty diffs
    /// have no patch id and return `None`.
    pub fn patch_id_between(&self, from: &str, to: &str) -> Result<Option<String>, GitError> {
        let diff = Command::new("git")
            .args(["diff", "--binary", from, to, "--"])
            .current_dir(&self.root)
            .output()
            .map_err(|source| GitError::Spawn {
                args: "diff --binary".into(),
                source,
            })?;
        if !diff.status.success() {
            return Err(GitError::Git {
                args: "diff --binary".into(),
                stderr: String::from_utf8_lossy(&diff.stderr).trim().to_string(),
            });
        }
        if diff.stdout.is_empty() {
            return Ok(None);
        }
        let mut child = Command::new("git")
            .args(["patch-id", "--stable"])
            .current_dir(&self.root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|source| GitError::Spawn {
                args: "patch-id --stable".into(),
                source,
            })?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| GitError::Git {
                args: "patch-id --stable".into(),
                stderr: "failed to open patch-id stdin".into(),
            })?
            .write_all(&diff.stdout)
            .map_err(|source| GitError::Spawn {
                args: "patch-id --stable stdin".into(),
                source,
            })?;
        let output = child.wait_with_output().map_err(|source| GitError::Spawn {
            args: "patch-id --stable".into(),
            source,
        })?;
        if !output.status.success() {
            return Err(GitError::Git {
                args: "patch-id --stable".into(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next()
            .map(str::to_string))
    }

    /// Whether two commits have identical content for the supplied paths.
    pub fn paths_equal(&self, left: &str, right: &str, paths: &[String]) -> Result<bool, GitError> {
        if paths.is_empty() {
            return Ok(true);
        }
        let mut args = vec![
            "diff".to_string(),
            "--quiet".to_string(),
            left.into(),
            right.into(),
            "--".into(),
        ];
        args.extend(paths.iter().cloned());
        let status = Command::new("git")
            .args(&args)
            .current_dir(&self.root)
            .status()
            .map_err(|source| GitError::Spawn {
                args: "diff --quiet".into(),
                source,
            })?;
        match status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(GitError::Git {
                args: "diff --quiet".into(),
                stderr: "git diff could not compare reconciled paths".into(),
            }),
        }
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

    /// Replay `old_base..incoming` onto `current` using an explicit merge
    /// base. This preserves upstream follow-up fixes and never asks Git to
    /// infer history across rewritten (for example squash-merged) commits.
    pub fn merge_tree_with_base(
        &self,
        old_base: &str,
        current: &str,
        incoming: &str,
    ) -> Result<MergeSimulation, GitError> {
        let output = Command::new("git")
            .args([
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                "--merge-base",
                old_base,
                current,
                incoming,
            ])
            .current_dir(&self.root)
            .output()
            .map_err(|source| GitError::Spawn {
                args: "merge-tree --write-tree --merge-base".into(),
                source,
            })?;
        let code = output.status.code().unwrap_or(-1);
        if code != 0 && code != 1 {
            return Err(GitError::Git {
                args: "merge-tree --write-tree --merge-base".into(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines();
        let tree = lines.next().unwrap_or_default().trim().to_string();
        let conflicts = lines
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
        Ok(parse_path_lines(&run_git(
            &self.root,
            &["diff", "--name-only", from, to],
        )?))
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
        let index_dir = self.git_common_dir()?.join("aethyme-broker/indexes");
        std::fs::create_dir_all(&index_dir).map_err(|source| GitError::Spawn {
            args: format!("create_dir_all {}", index_dir.display()),
            source,
        })?;
        let tmp = index_dir.join(format!(
            "index-{}-{}.tmp",
            std::process::id(),
            NEXT_PRIVATE_INDEX_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp_str = tmp.to_string_lossy().into_owned();
        let lock = tmp.with_file_name(format!(
            "{}.lock",
            tmp.file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default()
        ));
        let result = (|| {
            run_git_with_index(&self.root, &tmp_str, &["read-tree", "HEAD"])?;
            run_git_with_index(&self.root, &tmp_str, &["add", "-A", "."])?;
            run_git_with_index(&self.root, &tmp_str, &["write-tree"])
        })();
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(lock);
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

    /// Untracked paths from porcelain status. Used for the adoption-time
    /// foreign-file snapshot; keeping the status grammar here prevents
    /// output decoration from becoming "foreign" broker state.
    pub fn untracked_paths(&self) -> Result<Vec<String>, GitError> {
        Ok(parse_porcelain_entries(&run_git(
            &self.root,
            &["status", "--porcelain", "--untracked-files=all"],
        )?)
        .into_iter()
        .filter(|(xy, _path)| *xy == [b'?', b'?'])
        .map(|(_xy, path)| path)
        .collect())
    }

    /// Fetch a local commit object into this worktree's repository. Used
    /// by broker repair to apply the same no-network path written into
    /// `.aethyme/broker-action-required.md`.
    pub fn fetch_local_commit(&self, commit: &str) -> Result<(), GitError> {
        run_git(&self.root, &["fetch", ".", commit])?;
        Ok(())
    }

    /// Rebase this checkout onto `base`. If git stops for conflicts, the
    /// caller receives the stderr and the worktree is intentionally left
    /// in the paused rebase state for manual resolution.
    pub fn rebase_onto(&self, base: &str) -> Result<(), GitError> {
        run_git(&self.root, &["rebase", base])?;
        Ok(())
    }

    /// Replay exactly `upstream..HEAD` onto `base`. Unlike a plain
    /// `git rebase <base>`, this never lets Git infer an older merge-base
    /// and accidentally include commits that predate the broker session.
    pub fn rebase_onto_range(&self, base: &str, upstream: &str) -> Result<(), GitError> {
        run_git(&self.root, &["rebase", "--onto", base, upstream])?;
        Ok(())
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

    /// Commits reachable from `to` but not from `from`. Used by status
    /// surfaces that compare named refs instead of this checkout's HEAD.
    pub fn commit_count_between(&self, from: &str, to: &str) -> Result<u64, GitError> {
        let count = run_git(
            &self.root,
            &["rev-list", "--count", &format!("{from}..{to}")],
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
