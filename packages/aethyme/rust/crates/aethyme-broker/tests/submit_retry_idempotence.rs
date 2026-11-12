//! Issue #135: a submit whose response is lost is safe to retry. Nothing is
//! promoted; a retry verifies the same head again and leaves one live entry.

use std::path::Path;
use std::process::Command;

use aethyme_broker::MergeStatus;

mod common;

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

fn repo_with_session(
    tmp: &Path,
) -> (
    std::path::PathBuf,
    common::FixtureBroker,
    i64,
    std::path::PathBuf,
) {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    sh(&repo, &["add", "-A"]);
    sh(&repo, &["commit", "-qm", "init"]);

    let mut broker = common::open_fixture_broker(&repo, tmp).unwrap();
    let session = broker.start_worktree("work", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("work.txt"), "landed\n").unwrap();
    sh(&worktree, &["add", "-A"]);
    sh(&worktree, &["commit", "-qm", "feat: work"]);
    (repo, broker, session.id, worktree)
}

/// A lost response: the caller retries the same head. The retry verifies it
/// again, moves nothing, and leaves exactly one live verified entry.
#[test]
fn a_retry_of_the_same_head_verifies_again_and_leaves_one_live_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, mut broker, session_id, _worktree) = repo_with_session(tmp.path());
    let main_before = git_output(&repo, &["rev-parse", "main"]);

    let first = broker.submit(session_id).unwrap();
    assert_eq!(first.entry.status, MergeStatus::Verified);
    assert!(!first.promoted);

    let retry = broker.submit(session_id).unwrap();
    assert_eq!(retry.entry.status, MergeStatus::Verified);
    assert!(!retry.promoted);
    assert_eq!(retry.entry.head_commit, first.entry.head_commit);

    let live_verified = broker
        .store()
        .merge_queue()
        .unwrap()
        .into_iter()
        .filter(|entry| entry.session_id == session_id && entry.status == MergeStatus::Verified)
        .count();
    assert_eq!(live_verified, 1, "a retry leaves one live verified entry");
    assert_eq!(
        git_output(&repo, &["rev-parse", "main"]),
        main_before,
        "submit never moves the default branch"
    );
}

/// Work another session already landed by pull request replays to nothing:
/// the submission is content-empty and superseded.
#[test]
fn work_landed_by_another_session_is_content_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, mut broker, _session_id, worktree) = repo_with_session(tmp.path());

    // A second session branches before either lands, and independently produces
    // the identical content.
    let other = broker.start_worktree("same work", None).unwrap();
    let other_worktree = std::path::PathBuf::from(&other.worktree_path);
    std::fs::write(other_worktree.join("work.txt"), "landed\n").unwrap();
    sh(&other_worktree, &["add", "-A"]);
    sh(&other_worktree, &["commit", "-qm", "feat: same work"]);

    // The first session lands it by pull request.
    let branch = git_output(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]);
    aethyme_testkit::land_as_pull_request(&repo, &branch, aethyme_testkit::MergeStyle::Squash);

    let outcome = broker.submit(other.id).unwrap();
    assert!(
        outcome.no_changes && !outcome.promoted,
        "content landed by another session is content-empty: {:?}",
        outcome.entry.status
    );
    assert_eq!(outcome.entry.status, MergeStatus::Superseded);
}

fn git_output(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
