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

/// A repository whose closed worktrees have no grace period unless `grace`
/// names one. `None` leaves the key out, so the shipped default applies.
fn repository(grace_hours: Option<u32>) -> (tempfile::TempDir, Broker) {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    let grace = grace_hours
        .map(|hours| format!("closed_worktree_grace_hours = {hours}\n"))
        .unwrap_or_default();
    std::fs::write(
        tmp.path().join(".aethyme/broker.toml"),
        format!("[retention]\nartifact_sweep_budget_ms = 0\nstartup_budget_ms = 5\n{grace}"),
    )
    .unwrap();
    let broker = Broker::open(tmp.path()).unwrap();
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
        tmp.path(),
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
