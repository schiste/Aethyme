//! End-to-end proof for #176: the broker can say what is on disk that it does
//! not own.
//!
//! The unit tests in `worktree_reconcile` cover the matching rule. These cover
//! the property that made ownership drift invisible in the first place -- every
//! cleanup surface reasons from session rows, so a directory with no row was
//! absent from the arithmetic rather than reported as unaccounted for. These
//! also pin the limit the sweep was given: it names directories, and nothing
//! removes them.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::Broker;

fn git(repo: &Path, args: &[&str]) {
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
}

fn fixture(tmp: &Path) {
    git(tmp, &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
    git(tmp, &["add", "-A"]);
    git(tmp, &["commit", "-qm", "init"]);
}

/// A session whose worktree is on disk, plus the root that worktree sits in.
///
/// The root is read back from the broker rather than assumed, because the
/// sweep's whole job is to scan the roots the broker actually uses.
fn session_and_root(broker: &mut Broker) -> (i64, PathBuf) {
    let session = broker.start_worktree("reconcile fixture", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    let root = worktree.parent().unwrap().to_path_buf();
    (session.id, root)
}

/// A directory nothing in the broker knows about, with some bytes in it.
fn plant(root: &Path, name: &str, git_marker: bool) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("payload.bin"), vec![b'x'; 4096]).unwrap();
    if git_marker {
        std::fs::write(path.join(".git"), "gitdir: /nowhere\n").unwrap();
    }
    path
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn drift_advice(broker: &mut Broker) -> Option<aethyme_broker::StatusAdvice> {
    broker
        .status(now_ms())
        .unwrap()
        .advice
        .into_iter()
        .find(|advice| advice.id == "cleanup.unclaimed-worktrees")
}

#[test]
fn a_directory_no_session_created_is_reported_rather_than_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let (_, root) = session_and_root(&mut broker);

    // Non-vacuity first: with only the session's own worktree there, the
    // broker accounts for everything it can see and says nothing.
    assert!(
        drift_advice(&mut broker).is_none(),
        "a root holding only claimed worktrees must not report drift"
    );

    let stray = plant(&root, "left-behind", false);
    let advice = drift_advice(&mut broker).expect("an unclaimed directory must be reported");
    assert!(
        advice
            .evidence
            .iter()
            .any(|line| line.contains(stray.to_string_lossy().as_ref())
                || line.contains("left-behind")),
        "the advice must name the directory, not just count it: {:?}",
        advice.evidence
    );
}

#[test]
fn the_session_that_owns_a_worktree_keeps_it_out_of_the_drift_count() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let (_, root) = session_and_root(&mut broker);
    plant(&root, "left-behind", false);

    let sweep = broker.reconcile_worktree_directories(false).unwrap();
    assert_eq!(
        sweep.directory_count, 2,
        "the sweep must see both worktree directories and neither of the \
         broker's own dotted directories: {:?}",
        sweep.unclaimed
    );
    assert_eq!(sweep.claimed_count, 1, "the live session claims its own");
    assert_eq!(sweep.unclaimed_count, 1);
    assert_eq!(sweep.unclaimed.len(), 1);
}

#[test]
fn a_closed_sessions_worktree_is_not_drift_merely_because_it_is_closed() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.start_worktree("reconcile fixture", None).unwrap();
    std::fs::write(
        PathBuf::from(&session.worktree_path).join("done.txt"),
        "done\n",
    )
    .unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    git(&worktree, &["add", "done.txt"]);
    git(&worktree, &["commit", "-qm", "done"]);
    assert!(broker.submit(session.id).unwrap().promoted);
    broker.close(session.id).unwrap();

    // A closed session still owns its worktree until cleanup removes it. If
    // the sweep only consulted live rows, every retained worktree in the
    // system would be reported as drift the moment its session closed.
    let sweep = broker.reconcile_worktree_directories(false).unwrap();
    assert_eq!(
        sweep.unclaimed_count, 0,
        "a retained worktree is owned, not drifted: {:?}",
        sweep.unclaimed
    );
    assert_eq!(sweep.claimed_count, 1);
}

#[test]
fn status_counts_the_drift_and_gc_plan_measures_it() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let (_, root) = session_and_root(&mut broker);
    plant(&root, "left-behind", false);

    // The routine path counts without walking: bytes are zero *and* it says
    // so, which is the distinction that stops a reader concluding the
    // directory is empty.
    let cheap = broker.reconcile_worktree_directories(false).unwrap();
    assert!(!cheap.sized);
    assert_eq!(cheap.unclaimed_bytes, 0);
    assert!(cheap.unclaimed[0].estimated_bytes.is_none());

    // And the routine surface that actually runs on every `broker status`
    // uses that cheap path, not the walk.
    let status = broker.status(now_ms()).unwrap();
    assert!(
        !status.cleanup_retention.reconciliation.sized,
        "status must not walk every unclaimed directory to answer"
    );
    assert_eq!(status.cleanup_retention.reconciliation.unclaimed_count, 1);

    let plan = broker.gc_plan().unwrap();
    let sweep = plan
        .reconciliation
        .as_ref()
        .expect("a freshly built plan always carries a sweep");
    assert!(
        sweep.sized,
        "the expensive path must report itself as sized"
    );
    assert!(
        sweep.unclaimed_bytes >= 4096,
        "the planted payload must be counted: {}",
        sweep.unclaimed_bytes
    );
}

#[test]
fn a_former_worktree_reads_differently_from_a_stray_directory() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let (_, root) = session_and_root(&mut broker);
    plant(&root, "was-a-worktree", true);
    plant(&root, "just-a-directory", false);

    let sweep = broker.reconcile_worktree_directories(false).unwrap();
    let kind = |name: &str| {
        sweep
            .unclaimed
            .iter()
            .find(|entry| entry.path.ends_with(name))
            .unwrap_or_else(|| panic!("{name} must be reported"))
            .kind
            .clone()
    };
    assert_eq!(kind("was-a-worktree"), "untracked_worktree");
    assert_eq!(kind("just-a-directory"), "stray_directory");
}

#[test]
fn no_lane_removes_an_unclaimed_directory() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let (_, root) = session_and_root(&mut broker);
    let stray = plant(&root, "left-behind", true);

    let plan = broker.gc_plan().unwrap();
    assert_eq!(
        plan.reconciliation.as_ref().unwrap().unclaimed_count,
        1,
        "non-vacuity: the plan must actually have found it"
    );
    assert!(
        !plan
            .worktrees
            .iter()
            .any(|item| stray.to_string_lossy().contains(&item.worktree_path)),
        "an unclaimed directory must never become a removal candidate"
    );
    let digest = plan.digest.clone();
    broker.gc_apply(&digest).unwrap();
    assert!(
        stray.join("payload.bin").exists(),
        "gc apply removed a directory it was only allowed to report"
    );
}

#[test]
fn drift_does_not_invalidate_an_operators_authorization() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let (_, root) = session_and_root(&mut broker);

    let before = broker.gc_plan().unwrap();
    plant(&root, "appeared-after-the-plan", false);
    let after = broker.gc_plan().unwrap();

    assert_eq!(
        after.reconciliation.as_ref().unwrap().unclaimed_count,
        1,
        "non-vacuity: the second plan must have seen the new directory"
    );
    assert_eq!(
        before.digest, after.digest,
        "reporting-only data must stay out of the digest, or a directory \
         appearing on disk would revoke an authorization to remove something else"
    );
}

#[test]
fn a_worktree_reached_by_another_spelling_is_still_owned() {
    // Two sessions can reach one repository by different paths -- a symlinked
    // checkout, `/var` against `/private/var` -- and they share one database.
    // If the sweep compared the strings it was handed, the session row written
    // through one spelling would match nothing read through the other, and a
    // perfectly healthy worktree would be reported as drift for a human to
    // investigate.
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("repo");
    std::fs::create_dir(&real).unwrap();
    fixture(&real);
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let mut through_link = Broker::open(&link).unwrap();
    let session = through_link
        .start_worktree("reconcile fixture", None)
        .unwrap();
    // The invariant the sweep depends on: whatever spelling reached the
    // broker, what lands in the row is the canonical path.
    assert!(
        !session.worktree_path.contains("/link/"),
        "the broker must canonicalise the root it was opened through, got {}",
        session.worktree_path
    );
    drop(through_link);

    let mut through_real = Broker::open(&real).unwrap();
    let sweep = through_real.reconcile_worktree_directories(false).unwrap();
    assert_eq!(
        sweep.unclaimed_count, 0,
        "a worktree named differently is the same worktree: {:?}",
        sweep.unclaimed
    );
    assert_eq!(sweep.claimed_count, 1);
}
