//! Recovery for a promotion whose queue record was lost mid-promotion (#130).

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, FinishOptions, MergeStatus};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A repository with one promoted entry, then the promotion record erased the
/// way an interrupted promotion erases it: status reverted, commit detail lost.
fn fixture() -> (tempfile::TempDir, Broker, i64, String) {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);

    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.start_worktree("delivered work").unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("work.txt"), "work\n").unwrap();
    git(&worktree, &["add", "work.txt"]);
    git(&worktree, &["commit", "-qm", "work"]);
    let outcome = broker.submit(session.id).unwrap();
    assert!(outcome.promoted, "fixture requires a promoted entry");
    let entry_id = outcome.entry.id;
    let promotion = git(tmp.path(), &["rev-parse", "refs/heads/aethyme/integration"]);
    broker
        .finish_with_options(session.id, FinishOptions { keep_worktree: false })
        .unwrap();
    (tmp, broker, entry_id, promotion)
}

/// Erase the promotion record exactly as an interrupted promotion does: the
/// commit stays on integration, the entry keeps its merged tree, but the status
/// and commit detail are gone.
fn erase_record(repo: &Path, entry_id: i64) {
    let db = repo.join(".aethyme/broker.db");
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute(
        "UPDATE merge_queue SET status='superseded',
         details_json='{\"reason\":\"submission produces no content change\"}'
         WHERE id = ?1",
        [entry_id],
    )
    .unwrap();
}

#[test]
fn a_lost_promotion_record_is_recovered_from_its_merge_tree() {
    let (tmp, broker, entry_id, promotion) = fixture();
    drop(broker); // close the fixture connection before reopening
    erase_record(tmp.path(), entry_id);

    let mut broker = Broker::open(tmp.path()).unwrap();
    let plan = broker.promotion_record_plan().unwrap();
    let recoverable: Vec<_> = plan.recoverable().collect();
    assert_eq!(recoverable.len(), 1, "expected one recoverable commit");
    let candidate = recoverable[0];
    assert_eq!(candidate.commit, promotion);
    assert_eq!(candidate.entry_id, Some(entry_id));
    assert_eq!(candidate.current_status.as_deref(), Some("superseded"));
    assert!(
        candidate
            .evidence
            .iter()
            .any(|e| e.contains("merged_tree")),
        "the merge tree is the load-bearing evidence: {:?}",
        candidate.evidence
    );

    let report = broker.promotion_record_apply(&plan.digest).unwrap();
    assert_eq!(report.restored, vec![entry_id]);

    // The restored record must match what a normal promotion writes.
    let entry = broker
        .store()
        .merge_queue()
        .unwrap()
        .into_iter()
        .find(|e| e.id == entry_id)
        .unwrap();
    assert_eq!(entry.status, MergeStatus::Promoted);
    let details: serde_json::Value =
        serde_json::from_str(entry.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(details["commit"], promotion);
    assert_eq!(details["branch"], "aethyme/integration");

    // Nothing left to recover.
    let after = broker.promotion_record_plan().unwrap();
    assert_eq!(after.recoverable().count(), 0);
    drop(broker);
    drop(tmp);
}

#[test]
fn a_stale_digest_is_refused() {
    let (tmp, b, entry_id, _p) = fixture();
    drop(b);
    erase_record(tmp.path(), entry_id);
    let mut broker = Broker::open(tmp.path()).unwrap();
    let err = broker
        .promotion_record_apply(&"0".repeat(64))
        .expect_err("a digest that does not match the current plan must be refused");
    assert!(
        format!("{err}").contains("confirmation mismatch"),
        "unexpected error: {err}"
    );
    // Refusal must not have written anything.
    let plan = broker.promotion_record_plan().unwrap();
    assert_eq!(plan.recoverable().count(), 1);
}

#[test]
fn a_healthy_repository_has_nothing_to_recover() {
    let (tmp, b, _e, _p) = fixture();
    drop(b);
    let mut broker = Broker::open(tmp.path()).unwrap();
    let plan = broker.promotion_record_plan().unwrap();
    assert_eq!(
        plan.recoverable().count(),
        0,
        "a correctly recorded promotion must never be offered for repair"
    );
}

#[test]
fn an_unmatched_commit_is_reported_but_never_repaired() {
    let (tmp, b, entry_id, _p) = fixture();
    drop(b);
    erase_record(tmp.path(), entry_id);
    // Destroy the one piece of load-bearing evidence.
    let conn = rusqlite::Connection::open(tmp.path().join(".aethyme/broker.db")).unwrap();
    conn.execute("UPDATE merge_queue SET merged_tree=NULL WHERE id=?1", [entry_id])
        .unwrap();
    drop(conn);

    let mut broker = Broker::open(tmp.path()).unwrap();
    let plan = broker.promotion_record_plan().unwrap();
    assert_eq!(
        plan.recoverable().count(),
        0,
        "without a merge tree there is no proof, so nothing may be repaired"
    );
    assert!(
        plan.candidates.iter().any(|c| c.blocker.is_some()),
        "the unrecorded commit must still be reported"
    );
}
