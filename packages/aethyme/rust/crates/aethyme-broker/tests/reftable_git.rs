//! Reftable fixtures for the first-party Git boundary.

use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::GitRepo;

fn git_output(root: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    command.args(args).current_dir(root);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "reftable fixture")
        .env("GIT_AUTHOR_EMAIL", "reftable@example.test")
        .env("GIT_COMMITTER_NAME", "reftable fixture")
        .env("GIT_COMMITTER_EMAIL", "reftable@example.test")
        .output()
        .expect("run git")
}

fn run_git(root: &Path, args: &[&str]) {
    let output = git_output(root, args);
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_reftable_repo(root: &Path) {
    let output = git_output(root, &["init", "-q", "--ref-format=reftable", "-b", "main"]);
    assert!(
        output.status.success(),
        "this fixture requires Git with reftable support: `git init --ref-format=reftable -b main` failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );

    let storage = git_output(root, &["config", "--local", "--get", "extensions.refStorage"]);
    assert!(
        storage.status.success(),
        "Git accepted --ref-format=reftable but did not record extensions.refStorage: {}",
        String::from_utf8_lossy(&storage.stderr).trim()
    );
    assert_eq!(
        String::from_utf8_lossy(&storage.stdout).trim(),
        "reftable",
        "the fixture must use Git's reftable ref store"
    );
}

fn reftable_repo() -> (tempfile::TempDir, GitRepo, String, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    init_reftable_repo(root);

    std::fs::write(root.join("README.md"), "first\n").expect("write first commit");
    run_git(root, &["add", "README.md"]);
    run_git(root, &["commit", "-qm", "first"]);
    let repo = GitRepo::discover(root).expect("discover reftable repo");
    let first = repo
        .resolve_ref("refs/heads/main")
        .expect("first commit ref");
    run_git(root, &["branch", "agent/kept"]);

    std::fs::write(root.join("README.md"), "second\n").expect("write second commit");
    run_git(root, &["commit", "-qam", "second"]);
    run_git(root, &["branch", "agent/moved"]);
    let second = repo
        .resolve_ref("refs/heads/main")
        .expect("second commit ref");

    (tmp, repo, first, second)
}

#[test]
fn reftable_local_branch_tips_matches_every_ref_resolve_reports() {
    let (_tmp, repo, first, second) = reftable_repo();

    let tips = repo.local_branch_tips().expect("list local branch tips");
    for name in [
        "refs/heads/main",
        "refs/heads/agent/kept",
        "refs/heads/agent/moved",
    ] {
        assert_eq!(
            tips.get(name),
            repo.resolve_ref(name).as_ref(),
            "the boundary inventory should match Git's exact ref resolution for {name}"
        );
    }
    assert_eq!(tips["refs/heads/main"], second);
    assert_eq!(tips["refs/heads/agent/kept"], first);
    assert_eq!(tips["refs/heads/agent/moved"], second);
    assert_eq!(tips.len(), 3, "unexpected refs in inventory: {tips:?}");
    assert!(!tips.contains_key("refs/heads/agent/gone"));
    assert!(repo.resolve_ref("refs/heads/agent/gone").is_none());
}

#[test]
fn reftable_checked_branch_update_rejects_stale_expected_old_and_can_restore() {
    let (_tmp, repo, first, second) = reftable_repo();
    let branch = "agent/kept";
    let reference = format!("refs/heads/{branch}");

    repo.update_branch_ref_checked(branch, &second, &first)
        .expect("advance branch from its exact old tip");
    assert_eq!(repo.resolve_ref(&reference).as_deref(), Some(second.as_str()));

    assert!(
        repo.update_branch_ref_checked(branch, &first, &first)
            .is_err(),
        "a stale expected old value must not overwrite the branch"
    );
    assert_eq!(
        repo.resolve_ref(&reference).as_deref(),
        Some(second.as_str()),
        "the rejected compare-and-swap must leave the branch untouched"
    );

    repo.update_branch_ref_checked(branch, &first, &second)
        .expect("restore branch from its exact updated tip");
    assert_eq!(repo.resolve_ref(&reference).as_deref(), Some(first.as_str()));

    let tips = repo.local_branch_tips().expect("list restored branch tips");
    for name in [
        "refs/heads/main",
        "refs/heads/agent/kept",
        "refs/heads/agent/moved",
    ] {
        assert_eq!(
            tips.get(name),
            repo.resolve_ref(name).as_ref(),
            "the post-rollback inventory should match Git's exact ref resolution for {name}"
        );
    }
    assert_eq!(tips[&reference], first);
    assert_eq!(tips.len(), 3, "unexpected refs after rollback: {tips:?}");
}
