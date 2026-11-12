//! Land a session's work the way a merged pull request does.
//!
//! Pull requests are the only landing path: a session pushes its branch, the
//! pull request merges on the provider, and the session's commits reach the
//! default branch there. The local repository learns of it only by fetching:
//! `refs/remotes/origin/<default>` moves, while the local default branch and
//! the primary checkout stay where they were until someone pulls. That fetched
//! upstream is one of the delivery targets finish, cleanup and GC judge a
//! session's work against, by ancestry for a merge commit and by content for a
//! squash.
//!
//! [`land_as_pull_request`] produces exactly that state with plain Git and no
//! provider: it gives the fixture a local bare `origin` when it has none,
//! pushes the session branch, merges it into the remote default branch from a
//! scratch clone of the remote, and fetches.

use std::path::{Path, PathBuf};
use std::process::Command;

/// How the pull request merged. Landed-work detection handles both: a merge
/// commit keeps the session head reachable, a squash rewrites it and leaves
/// only the content behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStyle {
    /// `gh pr merge --merge`: a merge commit whose second parent is the
    /// session head.
    MergeCommit,
    /// `gh pr merge --squash`: one new commit carrying the session's net
    /// change; the session head is reachable from no remote default branch.
    Squash,
}

/// Where [`ensure_origin`] creates a bare remote for a fixture that has none:
/// inside the repository's Git directory, so it is never a tracked or
/// untracked path and goes away with the fixture.
pub const FIXTURE_ORIGIN_DIR: &str = "aethyme-test-origin.git";

fn git_command(cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    // A test can run under a Git hook or a parent repository's environment;
    // none of it may leak into the fixture's commands.
    let inherited = std::env::vars_os()
        .map(|(key, _)| key)
        .filter(|key| key.to_string_lossy().starts_with("GIT_"))
        .collect::<Vec<_>>();
    for key in inherited {
        command.env_remove(key);
    }
    command
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(cwd);
    command
}

fn try_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = git_command(cwd, args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = git_command(cwd, args)
        .output()
        .unwrap_or_else(|error| panic!("spawn git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The branch the primary checkout has out: the repository's default branch.
fn default_branch(repo: &Path) -> String {
    try_git(repo, &["symbolic-ref", "--short", "HEAD"]).unwrap_or_else(|| "main".to_string())
}

fn common_dir(repo: &Path) -> PathBuf {
    let common = PathBuf::from(git(repo, &["rev-parse", "--git-common-dir"]));
    if common.is_absolute() {
        common
    } else {
        repo.join(common)
    }
}

/// The local directory a remote URL names, if it names one.
fn local_remote_path(repo: &Path, url: &str) -> Option<PathBuf> {
    let path = PathBuf::from(url.strip_prefix("file://").unwrap_or(url));
    let path = if path.is_absolute() {
        path
    } else {
        repo.join(path)
    };
    path.is_dir().then_some(path)
}

/// The bare repository behind `repo`'s `origin`, creating one when there is
/// no `origin`. A created remote receives the current default branch, and the
/// local default branch tracks it, so `refs/remotes/origin/<default>` is a
/// configured, fetched upstream as it is in a clone.
///
/// Call it once the fixture's default branch holds everything it should start
/// from: commits made on the local default branch afterwards are not on the
/// remote, as they would not be after a real clone either.
///
/// Panics when `origin` exists but is not a local directory (a GitHub-shaped
/// URL behind a fake `ssh`): such a fixture merges with [`merge_on_remote`]
/// on its own bare path and fetches with its own environment.
pub fn ensure_origin(repo: &Path) -> PathBuf {
    if let Some(url) = try_git(repo, &["remote", "get-url", "origin"]) {
        return local_remote_path(repo, &url).unwrap_or_else(|| {
            panic!(
                "origin {url} is not a local directory; merge with \
                 aethyme_testkit::merge_on_remote on the fixture's bare path"
            )
        });
    }
    let default = default_branch(repo);
    let remote = common_dir(repo).join(FIXTURE_ORIGIN_DIR);
    std::fs::create_dir_all(&remote)
        .unwrap_or_else(|error| panic!("mkdir {}: {error}", remote.display()));
    git(&remote, &["init", "--bare", "-q", "-b", &default]);
    git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("UTF-8 path"),
        ],
    );
    git(repo, &["push", "-q", "-u", "origin", &default]);
    remote
}

/// Publish `branch` to `origin`, as `broker push` does: the remote gains the
/// branch and the local `refs/remotes/origin/<branch>` follows it.
pub fn push_branch(repo: &Path, branch: &str) {
    ensure_origin(repo);
    let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
    git(repo, &["push", "-q", "origin", &refspec]);
}

/// Merge `branch` into the default branch of the bare repository `remote`,
/// on the remote side, the way the provider merges a pull request. Returns
/// the remote default branch's new tip. Nothing in any local repository
/// moves; fetch to see it.
pub fn merge_on_remote(remote: &Path, branch: &str, style: MergeStyle) -> String {
    let scratch = tempfile::Builder::new()
        .prefix("aethyme-pr-merge-")
        .tempdir()
        .expect("create scratch directory");
    let clone = scratch.path().join("clone");
    git(
        scratch.path(),
        &[
            "clone",
            "-q",
            remote.to_str().expect("UTF-8 path"),
            clone.to_str().expect("UTF-8 path"),
        ],
    );
    // The bare repository's HEAD names its default branch; a remote created
    // without `-b` can name one that was never pushed, in which case the
    // fixture's branch is `main`.
    let default = try_git(remote, &["symbolic-ref", "--short", "HEAD"])
        .filter(|name| {
            try_git(
                remote,
                &["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")],
            )
            .is_some()
        })
        .unwrap_or_else(|| "main".to_string());
    git(
        &clone,
        &[
            "checkout",
            "-q",
            "-B",
            &default,
            &format!("origin/{default}"),
        ],
    );
    let source = format!("origin/{branch}");
    match style {
        MergeStyle::MergeCommit => {
            let message = format!("Merge pull request #1 from {branch}");
            git(&clone, &["merge", "-q", "--no-ff", "-m", &message, &source]);
        }
        MergeStyle::Squash => {
            let subject = git(&clone, &["log", "-1", "--format=%s", &source]);
            git(&clone, &["merge", "-q", "--squash", &source]);
            git(&clone, &["commit", "-q", "-m", &format!("{subject} (#1)")]);
        }
    }
    git(
        &clone,
        &[
            "push",
            "-q",
            "origin",
            &format!("HEAD:refs/heads/{default}"),
        ],
    );
    git(&clone, &["rev-parse", "HEAD"])
}

/// Delete `branch` on `origin` and prune its remote-tracking ref, as a
/// provider's "delete branch on merge" does. After a squash this leaves the
/// session head on no remote ref at all, so only content can prove it landed.
pub fn delete_remote_branch(repo: &Path, branch: &str) {
    git(repo, &["push", "-q", "origin", "--delete", branch]);
    git(repo, &["fetch", "-q", "--prune", "origin"]);
}

/// Fetch `origin`, so the remote-tracking refs show what merged there.
pub fn fetch_origin(repo: &Path) {
    git(repo, &["fetch", "-q", "origin"]);
}

/// Land `branch` the pull-request way: push it, merge it into `origin`'s
/// default branch on the remote with `style`, and fetch. Returns the commit
/// that landed it (the merge commit, or the squash commit).
///
/// Afterwards `refs/remotes/origin/<default>` carries the session's work and
/// `refs/remotes/origin/<branch>` its head, while the local default branch and
/// the primary checkout are unchanged: the state the broker sees after a pull
/// request merged on the provider. It records no broker state; a broker-run
/// `gh pr merge` additionally records the landing, which a test of that path
/// exercises on its own.
pub fn land_as_pull_request(repo: &Path, branch: &str, style: MergeStyle) -> String {
    let remote = ensure_origin(repo);
    push_branch(repo, branch);
    let landed = merge_on_remote(&remote, branch, style);
    fetch_origin(repo);
    landed
}
