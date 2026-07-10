//! Git service layer tests against real throwaway repositories (issue #7
//! groundwork / #10 dirty-protection guard).

use std::path::Path;
use std::process::Command;

use aethyme_broker::GitRepo;

fn sh(cwd: &Path, cmd: &str, args: &[&str]) {
    let status = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "{cmd} {args:?} failed");
}

fn init_repo(root: &Path) {
    sh(root, "git", &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    sh(root, "git", &["add", "-A"]);
    sh(root, "git", &["commit", "-qm", "init"]);
}

#[test]
fn discover_rejects_non_repos_and_finds_roots() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(GitRepo::discover(tmp.path()).is_err());

    init_repo(tmp.path());
    let repo = GitRepo::discover(tmp.path()).unwrap();
    assert_eq!(
        repo.root().canonicalize().unwrap(),
        tmp.path().canonicalize().unwrap()
    );
    assert_eq!(repo.current_branch().unwrap(), "main");
    assert_eq!(repo.head_commit().unwrap().len(), 40);
}

#[test]
fn worktree_lifecycle_and_main_root_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let repo = GitRepo::discover(tmp.path()).unwrap();

    let wt_path = tmp.path().join(".aethyme/worktrees/fix-auth");
    let worktree = repo
        .worktree_add(&wt_path, "agent/fix-auth", "main")
        .unwrap();
    assert_eq!(worktree.current_branch().unwrap(), "agent/fix-auth");

    // The load-bearing invariant: a worktree resolves broker state to the
    // MAIN checkout, so every agent coordinates through one database.
    assert_eq!(
        worktree.main_root().unwrap(),
        tmp.path().canonicalize().unwrap()
    );
    assert_eq!(
        repo.main_root().unwrap(),
        tmp.path().canonicalize().unwrap()
    );

    let listed = repo.worktree_paths().unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].ends_with("fix-auth"));

    repo.worktree_remove(&wt_path, false).unwrap();
    assert!(repo.worktree_paths().unwrap().is_empty());
}

#[test]
fn changed_files_covers_committed_staged_unstaged_and_untracked() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let repo = GitRepo::discover(tmp.path()).unwrap();
    let base = repo.head_commit().unwrap();

    // Committed change.
    std::fs::write(tmp.path().join("committed.rs"), "x\n").unwrap();
    sh(tmp.path(), "git", &["add", "-A"]);
    sh(tmp.path(), "git", &["commit", "-qm", "c"]);
    // Staged change.
    std::fs::write(tmp.path().join("staged.rs"), "x\n").unwrap();
    sh(tmp.path(), "git", &["add", "staged.rs"]);
    // Unstaged change to a tracked file.
    std::fs::write(tmp.path().join("README.md"), "changed\n").unwrap();
    // Untracked file.
    std::fs::write(tmp.path().join("untracked.rs"), "x\n").unwrap();

    let changed = repo.changed_files(&base).unwrap();
    assert_eq!(
        changed,
        vec!["README.md", "committed.rs", "staged.rs", "untracked.rs"]
    );
}

#[test]
fn dirty_and_unmerged_guards_for_cleanup() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let repo = GitRepo::discover(tmp.path()).unwrap();
    assert!(!repo.is_dirty().unwrap());

    let wt_path = tmp.path().join(".aethyme/worktrees/task");
    let worktree = repo.worktree_add(&wt_path, "agent/task", "main").unwrap();
    assert!(!worktree.is_dirty().unwrap());
    assert_eq!(worktree.unmerged_commit_count("main").unwrap(), 0);

    // Dirty worktree: non-force removal must fail (git enforces it), and
    // is_dirty reports it so the broker can refuse with a better message.
    std::fs::write(wt_path.join("wip.rs"), "x\n").unwrap();
    assert!(worktree.is_dirty().unwrap());
    assert!(repo.worktree_remove(&wt_path, false).is_err());

    // Unmerged commits are visible even after the tree is clean again.
    sh(&wt_path, "git", &["add", "-A"]);
    sh(&wt_path, "git", &["commit", "-qm", "wip"]);
    assert!(!worktree.is_dirty().unwrap());
    assert_eq!(worktree.unmerged_commit_count("main").unwrap(), 1);

    // Force removal discards it (callers gate this behind --force).
    repo.worktree_remove(&wt_path, true).unwrap();
    assert!(repo.worktree_paths().unwrap().is_empty());
}
