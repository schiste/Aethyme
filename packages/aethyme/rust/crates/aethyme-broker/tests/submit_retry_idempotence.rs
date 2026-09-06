//! Issue #135: a submit whose response is lost must not leave its promotion
//! unrecorded when the caller retries.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{Broker, MergeStatus};

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

fn repo_with_session(tmp: &Path) -> (std::path::PathBuf, Broker, i64, std::path::PathBuf) {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    sh(&repo, &["add", "-A"]);
    sh(&repo, &["commit", "-qm", "init"]);

    let mut broker = Broker::open(&repo).unwrap();
    let session = broker.start_worktree("work", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("work.txt"), "landed\n").unwrap();
    sh(&worktree, &["add", "-A"]);
    sh(&worktree, &["commit", "-qm", "feat: work"]);
    (repo, broker, session.id, worktree)
}

/// The damage this fixes: the promotion commit is on integration, but queue
/// revalidation superseded the row that would have claimed it. A retry used to
/// record content-empty supersession, leaving the commit unrecorded and
/// refusing publication.
#[test]
fn a_retry_claims_a_promotion_whose_row_was_lost() {
    let tmp = tempfile::tempdir().unwrap();
    let (repo, mut broker, session_id, _worktree) = repo_with_session(tmp.path());

    let first = broker.submit(session_id).unwrap();
    assert!(first.promoted);
    let promoted_commit = Command::new("git")
        .args(["rev-parse", "aethyme/integration"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let promoted_commit = String::from_utf8_lossy(&promoted_commit.stdout)
        .trim()
        .to_string();

    // Simulate the lost response: the ref is correct, but the row that claimed
    // it is superseded, exactly as queue revalidation would leave it.
    broker
        .store()
        .set_merge_status(first.entry.id, MergeStatus::Superseded, None, None)
        .unwrap();

    // The retry must claim the existing promotion rather than record nothing.
    let retry = broker.submit(session_id).unwrap();
    assert!(
        retry.promoted,
        "a retry over an unrecorded self-promotion must claim it, got {:?}",
        retry.entry.status
    );
    assert_eq!(retry.entry.status, MergeStatus::Promoted);

    // Integration did not move, and the commit is now claimed.
    let after = Command::new("git")
        .args(["rev-parse", "aethyme/integration"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&after.stdout).trim(),
        promoted_commit,
        "claiming must not move integration"
    );
    let claimed = broker
        .store()
        .merge_queue()
        .unwrap()
        .into_iter()
        .filter(|entry| entry.status == MergeStatus::Promoted)
        .filter_map(|entry| {
            let details: serde_json::Value =
                serde_json::from_str(entry.details_json.as_deref()?).ok()?;
            details.get("commit")?.as_str().map(str::to_string)
        })
        .any(|commit| commit == promoted_commit);
    assert!(
        claimed,
        "the integration commit must be claimed by a promoted entry"
    );
}

/// A genuinely content-empty submission, where another session legitimately
/// produced the tip, must still supersede rather than claim that promotion.
#[test]
fn an_ordinary_content_empty_submission_still_supersedes() {
    let tmp = tempfile::tempdir().unwrap();
    let (_repo, mut broker, session_id, _worktree) = repo_with_session(tmp.path());

    // A second session branches before either lands, and independently produces
    // the identical content.
    let other = broker.start_worktree("same work", None).unwrap();
    let other_worktree = std::path::PathBuf::from(&other.worktree_path);
    std::fs::write(other_worktree.join("work.txt"), "landed\n").unwrap();
    sh(&other_worktree, &["add", "-A"]);
    sh(&other_worktree, &["commit", "-qm", "feat: same work"]);

    // The first session lands it.
    assert!(broker.submit(session_id).unwrap().promoted);

    // The second replays to nothing. The tip names the other session, so this
    // must be content-empty rather than a claim.
    let outcome = broker.submit(other.id).unwrap();
    assert!(
        outcome.no_changes && !outcome.promoted,
        "content landed by another session is content-empty, not a claim: {:?}",
        outcome.entry.status
    );
    assert_eq!(outcome.entry.status, MergeStatus::Superseded);
}
