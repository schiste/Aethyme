//! Issue #143: reconciling a local default branch that carries work integration
//! does not, without raw ref surgery.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{Broker, MainReconcileDisposition};

fn sh(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repo whose default branch is discoverable offline, with one promoted
/// session so an integration branch exists.
fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let remote = tmp.path().join("remote.git");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&remote).unwrap();
    sh(&remote, &["init", "--bare", "-q", "-b", "main"]);
    sh(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.join("src.txt"), "base\n").unwrap();
    sh(&repo, &["add", "-A"]);
    sh(&repo, &["commit", "-qm", "init"]);
    sh(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    sh(&repo, &["push", "-qu", "origin", "main"]);
    sh(&repo, &["remote", "set-head", "origin", "main"]);

    let mut broker = Broker::open(&repo).unwrap();
    let session = broker.start_worktree("seed integration", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("seed.txt"), "seed\n").unwrap();
    sh(&worktree, &["add", "-A"]);
    sh(&worktree, &["commit", "-qm", "seed"]);
    assert!(broker.submit(session.id).unwrap().promoted);
    (tmp, repo)
}

/// Work whose content already reached integration through a squashed promotion
/// has a different SHA and is not an ancestor, so ancestry and patch ids both
/// miss it. Content comparison must not.
#[test]
fn content_already_on_integration_is_recognized_despite_a_different_sha() {
    let (_tmp, repo) = fixture();
    let mut broker = Broker::open(&repo).unwrap();
    let integration = broker.main_reconcile_plan().unwrap().integration_sha;

    // Reproduce integration's content on main as an independent commit.
    sh(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
    sh(&repo, &["add", "-A"]);
    sh(&repo, &["commit", "-qm", "same content, different commit"]);

    let plan = broker.main_reconcile_plan().unwrap();
    assert_eq!(plan.integration_sha, integration);
    assert_eq!(plan.commits.len(), 1, "one local-only commit");
    assert_eq!(
        plan.commits[0].disposition,
        MainReconcileDisposition::AlreadyRepresented,
        "content present on integration must be recognized: {}",
        plan.commits[0].evidence
    );
    assert!(plan.safe, "refusal: {:?}", plan.refusal);
}

/// Work that never reached integration must refuse the move.
#[test]
fn unrepresented_work_refuses_the_apply_and_says_what_is_missing() {
    let (_tmp, repo) = fixture();
    let mut broker = Broker::open(&repo).unwrap();
    sh(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("only-local.txt"), "never submitted\n").unwrap();
    sh(&repo, &["add", "-A"]);
    sh(&repo, &["commit", "-qm", "feat: local only"]);

    let plan = broker.main_reconcile_plan().unwrap();
    assert_eq!(plan.unrepresented().count(), 1);
    assert!(!plan.safe);
    let refusal = plan.refusal.clone().unwrap();
    assert!(
        refusal.contains("replay them through a broker session"),
        "{refusal}"
    );

    let error = broker
        .main_reconcile_apply(1, &plan.digest)
        .expect_err("unrepresented work must never be moved over");
    assert!(error.to_string().contains("not represented"), "{error}");
    // Nothing moved.
    assert_eq!(
        broker.main_reconcile_plan().unwrap().local_sha,
        plan.local_sha
    );
}

/// The safe case moves the branch and preserves the pre-move tip.
#[test]
fn a_represented_branch_moves_and_keeps_a_preservation_ref() {
    let (_tmp, repo) = fixture();
    let mut broker = Broker::open(&repo).unwrap();
    sh(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
    sh(&repo, &["add", "-A"]);
    sh(&repo, &["commit", "-qm", "same content, different commit"]);

    let plan = broker.main_reconcile_plan().unwrap();
    assert!(plan.safe, "refusal: {:?}", plan.refusal);
    let before = plan.local_sha.clone();

    let session = broker.start_worktree("reconcile main", None).unwrap();
    let report = broker
        .main_reconcile_apply(session.id, &plan.digest)
        .unwrap();
    assert_eq!(report.moved_from, before);
    assert_eq!(report.moved_to, plan.integration_sha);

    // The branch moved, and the pre-move tip survives under the preservation ref.
    let moved = broker.main_reconcile_plan().unwrap();
    assert_eq!(moved.local_sha, plan.integration_sha);
    let preserved = Command::new("git")
        .args(["rev-parse", &report.preservation_ref])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(preserved.status.success(), "preservation ref must exist");
    assert_eq!(
        String::from_utf8_lossy(&preserved.stdout).trim(),
        before,
        "the preservation ref must point at the pre-move tip"
    );
}

/// A stale confirmation must not move anything.
#[test]
fn a_stale_confirmation_is_refused_with_guidance() {
    let (_tmp, repo) = fixture();
    let mut broker = Broker::open(&repo).unwrap();
    sh(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("seed.txt"), "seed\n").unwrap();
    sh(&repo, &["add", "-A"]);
    sh(&repo, &["commit", "-qm", "same content, different commit"]);

    let error = broker
        .main_reconcile_apply(1, &"0".repeat(64))
        .expect_err("a stale digest must be refused");
    let rendered = error.to_string();
    assert!(
        rendered.contains("no longer matches current state")
            && rendered.contains("main reconcile plan"),
        "{rendered}"
    );
    assert!(!rendered.contains("expected"), "{rendered}");
}
