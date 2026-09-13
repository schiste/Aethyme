//! End-to-end proof for #176: a session whose agent is gone stops pinning its
//! worktree without a human reading a warning.
//!
//! The unit tests in `session_abandonment` cover the decision. These cover the
//! consequence, which is the part that was actually broken: a stale session was
//! never a cleanup *candidate*, so no retention policy could reach its disk.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{Broker, Event};

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn session_events(broker: &mut Broker, session_id: i64) -> Vec<Event> {
    broker
        .store()
        .events_after(0, 10_000)
        .unwrap()
        .into_iter()
        .filter(|event| event.session_id == Some(session_id))
        .collect()
}

const HOUR_MS: i64 = 60 * 60 * 1000;

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

/// Start a session and land its work, leaving the worktree on disk.
fn landed_session(broker: &mut Broker, tmp: &Path) -> i64 {
    let session = broker.start_worktree("abandonment fixture", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "done.txt"]);
    git(&worktree, &["commit", "-qm", "done"]);
    assert!(broker.submit(session.id).unwrap().promoted);
    let _ = tmp;
    session.id
}

#[test]
fn a_quiet_session_pins_its_worktree_until_the_window_elapses() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let id = landed_session(&mut broker, tmp.path());

    // This is the #176 symptom, reproduced: the session is stale by any
    // ordinary reading, and its worktree is still not a cleanup candidate.
    broker.agents(now_ms() + 3 * HOUR_MS).unwrap();
    let plan = broker.cleanup_plan().unwrap();
    assert_eq!(
        plan.worktrees.len(),
        0,
        "a merely stale session must not be reclaimable yet"
    );

    // Past the default 72h window, the same session is a candidate.
    broker.agents(now_ms() + 100 * HOUR_MS).unwrap();
    let plan = broker.cleanup_plan().unwrap();
    let item = plan
        .worktrees
        .iter()
        .find(|item| item.session_id == id)
        .expect("abandoned session must become a cleanup candidate");
    assert!(
        item.eligible(),
        "landed, clean work should be eligible once abandoned: {}",
        item.reason
    );
    assert!(
        plan.estimated_reclaimable_bytes > 0,
        "reclaimable bytes must stop reading as blocked"
    );
}

#[test]
fn abandonment_grants_candidacy_but_never_eligibility() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.start_worktree("dirty fixture", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    // Uncommitted work. Abandoning the session must not put it at risk.
    std::fs::write(worktree.join("wip.txt"), "unsaved\n").unwrap();

    broker.agents(now_ms() + 100 * HOUR_MS).unwrap();
    let plan = broker.cleanup_plan().unwrap();
    let item = plan
        .worktrees
        .iter()
        .find(|item| item.session_id == session.id)
        .expect("abandoned session must still be listed");
    assert!(
        !item.eligible(),
        "a dirty worktree must stay blocked after abandonment, got {:?}",
        item.disposition
    );
    assert!(
        worktree.join("wip.txt").exists(),
        "abandonment must not touch the tree"
    );
}

#[test]
fn a_zero_window_restores_the_unbounded_hold() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/broker.toml"),
        "[retention]\nsession_abandoned_after_hours = 0\n",
    )
    .unwrap();

    let mut broker = Broker::open(tmp.path()).unwrap();
    let id = landed_session(&mut broker, tmp.path());
    broker.agents(now_ms() + 10_000 * HOUR_MS).unwrap();
    let plan = broker.cleanup_plan().unwrap();
    assert!(
        !plan.worktrees.iter().any(|item| item.session_id == id),
        "an opted-out repository must keep the previous behaviour"
    );
}

#[test]
fn the_transition_is_recorded_for_audit() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let id = landed_session(&mut broker, tmp.path());
    broker.agents(now_ms() + 100 * HOUR_MS).unwrap();

    // Reclaiming someone's worktree without a human in the loop is only
    // acceptable if the decision is reconstructable afterwards.
    let events = session_events(&mut broker, id);
    let abandoned = events
        .iter()
        .find(|event| event.kind == "session.abandoned")
        .expect("abandonment must append session.abandoned");
    let payload = abandoned.payload_json.as_deref().unwrap_or_default();
    assert!(
        payload.contains("idle_ms") && payload.contains("abandon_after_ms"),
        "the event must carry the observation and the threshold: {payload}"
    );
}

#[test]
fn abandonment_is_idempotent_across_repeated_status_checks() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let id = landed_session(&mut broker, tmp.path());
    for _ in 0..3 {
        broker.agents(now_ms() + 100 * HOUR_MS).unwrap();
    }
    let events = session_events(&mut broker, id);
    let count = events
        .iter()
        .filter(|event| event.kind == "session.abandoned")
        .count();
    assert_eq!(
        count, 1,
        "status checks must not re-abandon a closed session"
    );
}

#[test]
fn a_running_agent_keeps_its_worktree_however_long_it_has_been_quiet() {
    // The worst thing this feature could do is reclaim the worktree of an
    // agent that is still working. An agent thinking for a long time looks
    // exactly like an abandoned one on every signal except the process
    // itself, so this pins liveness ahead of the clock at the level where
    // the worktree actually gets removed.
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker
        .start_agent("long-running agent", "sleep 300", None)
        .unwrap();
    let pid = session.pid.expect("a spawned agent has a pid");

    broker.agents(now_ms() + 10_000 * HOUR_MS).unwrap();
    let plan = broker.cleanup_plan().unwrap();
    let reaped = plan
        .worktrees
        .iter()
        .any(|item| item.session_id == session.id);

    // Kill before asserting so a failure does not leave the child behind.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    assert!(
        !reaped,
        "a session with a live process must never become a cleanup candidate"
    );
}

#[test]
fn abandoning_an_adopted_session_releases_the_leases_it_was_holding() {
    // An adopted session owns no broker worktree, so abandoning it frees no
    // disk. It frees something else that was also held without bound: the
    // implicit leases that make other agents' claims conflict.
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("adopted task")).unwrap();
    std::fs::write(tmp.path().join("README.md"), "edited\n").unwrap();

    broker.status(now_ms()).unwrap();
    assert!(
        broker
            .store()
            .active_leases()
            .unwrap()
            .iter()
            .any(|lease| lease.session_id == session.id),
        "fixture must actually hold a lease, or the assertion below is vacuous"
    );

    broker.agents(now_ms() + 100 * HOUR_MS).unwrap();
    assert!(
        broker
            .store()
            .active_leases()
            .unwrap()
            .iter()
            .all(|lease| lease.session_id != session.id),
        "an abandoned session must stop blocking other agents"
    );
}
