//! GC reclaims the checkout a state-only close leaves behind, and only that.
//!
//! `finish close` keeps the worktree and branch on purpose. An agent that
//! starts a fresh session per task and ends each with that close leaves one
//! full checkout per task on disk; in the repository behind this lane that
//! was 42 checkouts and 22 GB of tracked data, which no build-cache reclaim
//! can touch. These tests pin which of those checkouts `gc plan` proposes,
//! which it refuses and why, and that apply revalidates before removing.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, BrokerOpError, SessionCleanupState};

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

/// A temporary repository at `<tmp>/repo` whose session worktrees live in
/// `<tmp>/worktrees`.
///
/// The worktree root is pinned inside the fixture. Left to the environment, a
/// workstation's `AETHYME_WORKTREE_ROOT` put every test repository's root in
/// one shared container; GC then counted the other tests' roots as orphans,
/// and a root added between `gc plan` and `gc apply` changed the digest, so
/// the apply tests failed in parallel runs (#598).
struct Fixture(tempfile::TempDir);

impl Fixture {
    fn path(&self) -> PathBuf {
        self.0.path().join("repo")
    }

    fn open(&self) -> Broker {
        Broker::open(&self.path())
            .unwrap()
            .with_worktree_root(self.0.path().join("worktrees"))
    }
}

/// A repository whose closed worktrees have no grace period unless `grace`
/// names one. `None` leaves the key out, so the shipped default applies.
fn repository(grace_hours: Option<u32>) -> (Fixture, Broker) {
    let tmp = Fixture(tempfile::tempdir().unwrap());
    let repo = tmp.path();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    let grace = grace_hours
        .map(|hours| format!("closed_worktree_grace_hours = {hours}\n"))
        .unwrap_or_default();
    std::fs::write(
        repo.join(".aethyme/broker.toml"),
        format!("[retention]\nartifact_sweep_budget_ms = 0\nstartup_budget_ms = 5\n{grace}"),
    )
    .unwrap();
    let broker = tmp.open();
    (tmp, broker)
}

/// Start a session, commit one file, and optionally land it on integration.
fn session_with_commit(broker: &mut Broker, name: &str, land: bool) -> (i64, PathBuf) {
    let session = broker.start_worktree(name, None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join(format!("{name}.txt")), "work\n").unwrap();
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-qm", name]);
    if land {
        assert!(broker.submit(session.id).unwrap().promoted);
    }
    (session.id, worktree)
}

fn proposed(broker: &mut Broker) -> Vec<i64> {
    broker
        .gc_plan()
        .unwrap()
        .worktrees
        .iter()
        .map(|worktree| worktree.session_id)
        .collect()
}

/// Blocker kinds whose id is a session id. Ids are per kind -- an accepted
/// checkpoint's is a queue entry, a publication exposure's is its own row --
/// so a bare id match would mix namespaces.
const SESSION_WORKTREE_KINDS: &[&str] = &[
    "adopted_worktree",
    "closed_worktree_grace",
    "live_worktree",
    "retention_age",
    "unproven_contribution",
];

/// Kinds of the worktree blockers naming `session_id`.
fn blocker_kinds(broker: &mut Broker, session_id: i64) -> Vec<String> {
    broker
        .gc_plan()
        .unwrap()
        .blockers
        .into_iter()
        .filter(|blocker| {
            blocker.id == Some(session_id)
                && SESSION_WORKTREE_KINDS.contains(&blocker.kind.as_str())
        })
        .map(|blocker| blocker.kind)
        .collect()
}

#[test]
fn a_closed_clean_landed_worktree_is_proposed_and_removed_on_apply() {
    let (tmp, mut broker) = repository(Some(0));
    let (id, worktree) = session_with_commit(&mut broker, "landed", true);
    broker.close(id).unwrap();
    let closed = broker.store().session(id).unwrap();
    assert_eq!(closed.cleanup_state, SessionCleanupState::Closed);
    assert!(worktree.exists(), "a state-only close keeps the checkout");

    let plan = broker.gc_plan().unwrap();
    let candidate = plan
        .worktrees
        .iter()
        .find(|worktree| worktree.session_id == id)
        .expect("the closed, clean, landed checkout is proposed");
    assert!(candidate.estimated_bytes > 0, "the plan reports its bytes");

    let report = broker.gc_apply(&plan.digest).unwrap();
    assert!(report.complete, "{report:?}");
    assert_eq!(report.sessions_cleaned, vec![id]);
    assert!(report.reclaimed_bytes >= candidate.estimated_bytes);
    assert!(!worktree.exists());
    let cleaned = broker.store().session(id).unwrap();
    assert_eq!(cleaned.cleanup_state, SessionCleanupState::Cleaned);
    assert!(cleaned.cleanup_completed_at.is_some());
    let branch = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("refs/heads/{}", closed.branch))
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(!branch.status.success(), "the session branch goes with it");
}

#[test]
fn a_dirty_closed_worktree_is_refused_with_its_reason() {
    let (_tmp, mut broker) = repository(Some(0));
    let (id, worktree) = session_with_commit(&mut broker, "dirty", true);
    broker.close(id).unwrap();
    std::fs::write(worktree.join("scratch.txt"), "uncommitted\n").unwrap();

    assert!(!proposed(&mut broker).contains(&id));
    let plan = broker.gc_plan().unwrap();
    let blocker = plan
        .blockers
        .iter()
        .find(|blocker| {
            blocker.id == Some(id) && SESSION_WORKTREE_KINDS.contains(&blocker.kind.as_str())
        })
        .expect("the refusal is reported");
    assert!(blocker.reason.contains("uncommitted"), "{blocker:?}");
    assert!(worktree.join("scratch.txt").exists());
}

#[test]
fn an_unlanded_closed_worktree_is_not_proposed() {
    let (_tmp, mut broker) = repository(Some(0));
    let (id, worktree) = session_with_commit(&mut broker, "unlanded", false);
    broker.close(id).unwrap();

    assert!(!proposed(&mut broker).contains(&id));
    assert!(
        !blocker_kinds(&mut broker, id).is_empty(),
        "the refusal is reported"
    );
    assert!(worktree.exists());
}

#[test]
fn a_closed_worktree_inside_its_grace_period_is_not_proposed() {
    // No key: the shipped 24 hour default applies.
    let (_tmp, mut broker) = repository(None);
    let (id, worktree) = session_with_commit(&mut broker, "recent", true);
    broker.close(id).unwrap();

    assert!(!proposed(&mut broker).contains(&id));
    assert_eq!(blocker_kinds(&mut broker, id), ["closed_worktree_grace"]);
    // Explicit cleanup is the operator's own decision and does not wait.
    assert!(
        broker
            .cleanup_plan()
            .unwrap()
            .worktrees
            .iter()
            .any(|item| item.session_id == id && item.eligible())
    );
    assert!(worktree.exists());
}

#[test]
fn a_closed_worktree_a_live_session_reuses_is_not_proposed() {
    let (_tmp, mut broker) = repository(Some(0));
    let (id, worktree) = session_with_commit(&mut broker, "reused", true);
    broker.close(id).unwrap();
    let live = broker.adopt(&worktree, Some("follow-up")).unwrap();
    assert_ne!(live.id, id);

    assert!(!proposed(&mut broker).contains(&id));
    assert_eq!(blocker_kinds(&mut broker, id), ["live_worktree"]);
    assert!(worktree.exists());
}

#[test]
fn a_closed_adopted_worktree_is_reported_and_never_proposed() {
    let (tmp, mut broker) = repository(Some(0));
    let outside = tempfile::tempdir().unwrap();
    let checkout = outside.path().join("adopted");
    git(
        &tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "adopted-branch",
            checkout.to_str().unwrap(),
        ],
    );
    let adopted = broker.adopt(&checkout, Some("adopted work")).unwrap();
    broker.close(adopted.id).unwrap();

    assert!(!proposed(&mut broker).contains(&adopted.id));
    assert_eq!(blocker_kinds(&mut broker, adopted.id), ["adopted_worktree"]);
    assert!(checkout.exists());
}

#[test]
fn a_stale_digest_is_refused_and_removes_nothing() {
    let (_tmp, mut broker) = repository(Some(0));
    let (id, worktree) = session_with_commit(&mut broker, "stale", true);
    broker.close(id).unwrap();
    let plan = broker.gc_plan().unwrap();
    assert!(plan.worktrees.iter().any(|item| item.session_id == id));

    // The checkout changed after the plan was reviewed.
    std::fs::write(worktree.join("late.txt"), "written after review\n").unwrap();
    let refused = broker.gc_apply(&plan.digest).unwrap_err();
    assert!(
        matches!(refused, BrokerOpError::GcConfirmationMismatch { .. }),
        "{refused:?}"
    );
    assert!(worktree.join("late.txt").exists());
    let session = broker.store().session(id).unwrap();
    assert_eq!(session.cleanup_state, SessionCleanupState::Closed);
}

#[test]
fn status_and_doctor_count_closed_checkouts_still_on_disk() {
    let (_tmp, mut broker) = repository(None);
    let now = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    };
    let before = broker
        .status(now())
        .unwrap()
        .cleanup_retention
        .closed_worktrees;
    assert_eq!(before.count, 0);
    assert_eq!(before.command, None, "nothing to review, nothing advised");

    let (id, _worktree) = session_with_commit(&mut broker, "kept", true);
    broker.close(id).unwrap();
    // The full audit records the checkout's size, which status then reads.
    let plan = broker.gc_plan().unwrap();
    assert_eq!(plan.closed_worktrees.count, 1);
    assert_eq!(plan.closed_worktrees.unmeasured_count, 0);
    assert!(plan.closed_worktrees.estimated_bytes > 0);

    let status = broker
        .status(now())
        .unwrap()
        .cleanup_retention
        .closed_worktrees;
    assert_eq!(status.count, 1);
    assert_eq!(
        status.estimated_bytes,
        plan.closed_worktrees.estimated_bytes
    );
    assert_eq!(status.command.as_deref(), Some("aethyme broker gc plan"));
    let doctor = broker.gc_health().unwrap().closed_worktrees;
    assert_eq!(doctor, status);
}

fn set_closed_at(repo: &Path, session_id: i64, closed_at: i64) {
    let db = rusqlite::Connection::open(repo.join(".aethyme/broker.db")).unwrap();
    db.execute(
        "UPDATE sessions SET closed_at = ?2 WHERE id = ?1",
        rusqlite::params![session_id, closed_at],
    )
    .unwrap();
}

#[test]
fn cleanup_keeps_the_time_a_session_was_first_closed() {
    let (tmp, mut broker) = repository(Some(0));
    let (id, _worktree) = session_with_commit(&mut broker, "timed", true);
    broker.close(id).unwrap();
    drop(broker);
    set_closed_at(&tmp.path(), id, 1_000);

    let mut broker = tmp.open();
    broker.cleanup(id, false).unwrap();
    let cleaned = broker.store().session(id).unwrap();
    assert_eq!(cleaned.cleanup_state, SessionCleanupState::Cleaned);
    assert_eq!(
        cleaned.closed_at,
        Some(1_000),
        "cleanup moved the close time"
    );
    assert!(cleaned.cleanup_completed_at.unwrap() > 1_000);
}

#[test]
fn closing_again_does_not_restart_the_grace_period() {
    let (tmp, mut broker) = repository(None);
    let (id, _worktree) = session_with_commit(&mut broker, "reclosed", true);
    broker.close(id).unwrap();
    drop(broker);
    let two_days_ago = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        - 48 * 3_600_000;
    set_closed_at(&tmp.path(), id, two_days_ago);

    let mut broker = tmp.open();
    broker.close(id).unwrap();
    assert_eq!(
        broker.store().session(id).unwrap().closed_at,
        Some(two_days_ago)
    );
    assert!(proposed(&mut broker).contains(&id));
}

/// A landed session closed state-only whose directory a live session then
/// adopted: the closed row still proves "clean and landed", but about the
/// live session's checkout.
fn closed_then_adopted(broker: &mut Broker, name: &str) -> (i64, i64, PathBuf) {
    let (id, worktree) = session_with_commit(broker, name, true);
    broker.close(id).unwrap();
    let live = broker.adopt(&worktree, Some("follow-up")).unwrap();
    (id, live.id, worktree)
}

#[test]
fn bulk_cleanup_refuses_a_closed_row_whose_checkout_a_live_session_adopted() {
    let (_tmp, mut broker) = repository(Some(0));
    let (id, live_id, worktree) = closed_then_adopted(&mut broker, "bulk");
    let plan = broker.cleanup_cleaned_worktrees(false, None).unwrap().plan;
    assert!(plan.worktrees.iter().any(|item| item.session_id == id));

    let report = broker
        .cleanup_cleaned_worktrees(true, Some(&plan.digest))
        .unwrap();
    assert!(report.removed_session_ids.is_empty(), "{report:?}");
    assert_eq!(report.failures.len(), 1);
    assert!(
        report.failures[0]
            .reason
            .contains(&format!("live session {live_id}")),
        "{:?}",
        report.failures
    );
    assert!(worktree.join("bulk.txt").exists());
}

#[test]
fn single_cleanup_refuses_a_live_checkout_even_when_forced() {
    let (_tmp, mut broker) = repository(Some(0));
    let (id, live_id, worktree) = closed_then_adopted(&mut broker, "single");
    for force in [false, true] {
        let refused = broker.cleanup(id, force).unwrap_err();
        assert!(
            matches!(
                refused,
                BrokerOpError::WorktreeInUseByLiveSession { id: refused_id, live_id: refused_live, .. }
                    if refused_id == id && refused_live == live_id
            ),
            "force={force}: {refused:?}"
        );
    }
    assert!(worktree.join("single.txt").exists());
    assert_eq!(
        broker.store().session(id).unwrap().cleanup_state,
        SessionCleanupState::Closed
    );
}

#[test]
fn a_resumed_gc_apply_retains_a_checkout_adopted_after_review() {
    let (_tmp, mut broker) = repository(Some(0));
    let (id, worktree) = session_with_commit(&mut broker, "resumed", true);
    broker.close(id).unwrap();
    let plan = broker.gc_plan().unwrap();
    assert!(plan.worktrees.iter().any(|item| item.session_id == id));
    // A zero budget authorizes and journals the plan but removes nothing, so
    // the adopt lands between review and removal.
    let started = broker.gc_apply_bounded(&plan.digest, Some(0)).unwrap();
    assert!(!started.complete);
    assert!(started.sessions_cleaned.is_empty());
    broker.adopt(&worktree, Some("follow-up")).unwrap();

    let resumed = broker.gc_apply(&plan.digest).unwrap();
    assert!(!resumed.sessions_cleaned.contains(&id), "{resumed:?}");
    assert!(
        resumed
            .failures
            .iter()
            .any(|failure| failure.contains("live session")),
        "{resumed:?}"
    );
    assert!(worktree.join("resumed.txt").exists());
}
