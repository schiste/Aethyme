//! Being over `retained_bytes_budget` has to change what the broker does, not
//! only what it reports (#176).
//!
//! Two claims are exercised end to end here, because both were previously true
//! only in principle: the plan's candidate order follows the budget, and the
//! plan says whether applying all of it would actually get back under budget.
//!
//! Order is not cosmetic. `gc apply --budget-ms` drains the plan's lists in
//! order and stops at its deadline, so the front of the list is the part of a
//! bounded sweep that is real.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, FinishOptions, ReclaimOrder};

const DAY_MS: i64 = 86_400_000;

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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// `artifact_sweep_budget_ms = 0` disables the autonomous startup sweep: it
/// would otherwise reclaim the fixture's build caches on the next
/// `Broker::open`, and this test needs them to still be there to be ordered.
fn write_policy(main_root: &Path, retained_bytes_budget: u64) {
    std::fs::create_dir_all(main_root.join(".aethyme")).unwrap();
    std::fs::write(
        main_root.join(".aethyme/broker.toml"),
        format!(
            "[retention]\nterminal_events_days = 1\ngate_results_days = 1\nterminal_merge_queue_days = 1\ncommand_metrics_days = 1\nclosed_worktrees_days = 1\nartifact_reclaim_days = 1\nartifact_sweep_budget_ms = 0\nstartup_budget_ms = 5\nretained_bytes_budget = {retained_bytes_budget}\n"
        ),
    )
    .unwrap();
}

fn fixture() -> (tempfile::TempDir, Broker) {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    write_policy(tmp.path(), 0);
    let broker = Broker::open(tmp.path()).unwrap();
    (tmp, broker)
}

/// Deliver a session carrying `payload_bytes` of committed content, then close
/// it keeping the worktree, so it lands in the plan as a reclaim candidate.
fn deliver(broker: &mut Broker, task: &str, payload_bytes: usize) -> i64 {
    let session = broker.start_worktree(task, None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    // The size doubles as the content, so every session must ask for a
    // different one -- two identical payloads would make the second commit
    // empty and git would refuse it.
    std::fs::write(worktree.join("payload.bin"), vec![b'x'; payload_bytes]).unwrap();
    git(&worktree, &["add", "payload.bin"]);
    git(&worktree, &["commit", "-qm", task]);
    assert!(broker.submit(session.id).unwrap().promoted);
    assert!(
        broker
            .finish_with_options(
                session.id,
                FinishOptions {
                    keep_worktree: true
                }
            )
            .unwrap()
            .closed
    );
    session.id
}

/// Two retained worktrees on which age and size disagree: the *smaller* one is
/// the *older* one. That disagreement is the whole test -- if both rules
/// pointed the same way, neither assertion below could tell them apart.
///
/// Closing times are written directly because a test cannot wait a day, and
/// the age policy is not what is under test.
fn two_candidates(tmp: &tempfile::TempDir, broker: Broker) -> (Broker, i64, i64, i64, i64) {
    let mut broker = broker;
    let small = deliver(&mut broker, "small old delivery", 1_024);
    let large = deliver(&mut broker, "large new delivery", 512 * 1_024);
    // Two more retentions that whole-worktree cleanup can never reclaim:
    // closed, but left dirty. They serve two purposes. Their bytes count
    // toward the budget while staying off the worktree candidate list, which
    // is what makes an unmeetable budget reproducible at all; and because
    // build caches are reclaimed independently of a worktree's disposition,
    // they are the only sessions that can contribute artifact candidates.
    //
    // Their caches disagree the same way the worktrees do: the older session
    // holds the smaller one.
    let blocked_old = deliver(&mut broker, "blocked old delivery", 2_048);
    let blocked_new = deliver(&mut broker, "blocked new delivery", 3_072);
    for (id, cache_bytes) in [(blocked_old, 8 * 1_024), (blocked_new, 256 * 1_024)] {
        let worktree = PathBuf::from(&broker.store().session(id).unwrap().worktree_path);
        std::fs::write(worktree.join("scratch.bin"), vec![b'y'; 64 * 1_024]).unwrap();
        let cache = worktree.join("target");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
        std::fs::write(cache.join("build.bin"), vec![b'z'; cache_bytes]).unwrap();
    }
    drop(broker);

    let now = now_ms();
    let db = rusqlite::Connection::open(tmp.path().join(".aethyme/broker.db")).unwrap();
    for (id, age_days) in [(small, 10), (large, 5), (blocked_old, 20), (blocked_new, 3)] {
        let at = now - age_days * DAY_MS;
        db.execute(
            "UPDATE sessions SET closed_at = ?1, updated_at = ?1 WHERE id = ?2",
            rusqlite::params![at, id],
        )
        .unwrap();
    }
    drop(db);
    (
        Broker::open(tmp.path()).unwrap(),
        small,
        large,
        blocked_old,
        blocked_new,
    )
}

fn worktree_order(plan: &aethyme_broker::GcPlan) -> Vec<i64> {
    plan.worktrees.iter().map(|w| w.session_id).collect()
}

#[test]
fn within_budget_the_longest_retained_candidate_is_planned_first() {
    let (tmp, broker) = fixture();
    let (mut broker, small, large, _, _) = two_candidates(&tmp, broker);
    write_policy(tmp.path(), 0);

    let plan = broker.gc_plan().unwrap();
    assert_eq!(plan.reclaim_order, ReclaimOrder::OldestFirst);
    assert_eq!(plan.retained_bytes_deficit, 0);
    assert_eq!(
        worktree_order(&plan),
        vec![small, large],
        "no budget pressure means age decides"
    );
}

#[test]
fn over_budget_the_largest_candidate_is_planned_first() {
    let (tmp, broker) = fixture();
    let (mut broker, small, large, _, _) = two_candidates(&tmp, broker);
    // One byte of budget: anything retained at all is over it.
    write_policy(tmp.path(), 1);

    let plan = broker.gc_plan().unwrap();
    assert_eq!(plan.reclaim_order, ReclaimOrder::LargestFirst);
    assert_eq!(
        worktree_order(&plan),
        vec![large, small],
        "over budget, bytes decide -- even though the large one is the newer"
    );
}

#[test]
fn the_same_backlog_is_ordered_differently_by_the_budget_alone() {
    let (tmp, broker) = fixture();
    let (mut broker, _, _, _, _) = two_candidates(&tmp, broker);

    write_policy(tmp.path(), 0);
    let relaxed = worktree_order(&broker.gc_plan().unwrap());
    write_policy(tmp.path(), 1);
    let pressed = worktree_order(&broker.gc_plan().unwrap());

    assert_eq!(relaxed.len(), 2, "both candidates must reach the plan");
    assert_ne!(
        relaxed, pressed,
        "the budget must be the thing that moved the order; nothing else changed"
    );
}

#[test]
fn the_deficit_is_reported_as_bytes_to_shed_rather_than_a_flag() {
    let (tmp, broker) = fixture();
    let (mut broker, _, _, _, _) = two_candidates(&tmp, broker);
    write_policy(tmp.path(), 1);

    let plan = broker.gc_plan().unwrap();
    assert!(plan.estimated_retained_bytes > 1);
    assert_eq!(
        plan.retained_bytes_deficit,
        plan.estimated_retained_bytes - 1
    );
}

#[test]
fn a_budget_reclamation_cannot_meet_reads_differently_from_a_backlog() {
    let (tmp, broker) = fixture();
    let (mut broker, _, _, _, _) = two_candidates(&tmp, broker);

    // The dividing line is the blocked bytes: nothing reclaims those, so a
    // budget below them can never be met however much cleanup runs. This is
    // the #176 shape -- megabytes reclaimable against gigabytes retained.
    let measured = broker.gc_plan().unwrap();
    assert!(
        measured.estimated_blocked_bytes > 0 && measured.estimated_reclaimable_bytes > 0,
        "fixture must hold both reclaimable and blocked bytes for this to mean anything"
    );
    write_policy(tmp.path(), measured.estimated_blocked_bytes / 2);
    let unmeetable = broker.gc_plan().unwrap();
    assert!(unmeetable.retained_bytes_deficit > 0);
    assert!(
        !unmeetable.clears_retained_bytes_budget,
        "a plan that leaves more than the budget behind must not claim to clear it"
    );

    // A budget just above the blocked bytes: shedding the candidates covers
    // the gap. Same over-budget flag, entirely different situation.
    write_policy(tmp.path(), measured.estimated_blocked_bytes + 1);
    let backlog = broker.gc_plan().unwrap();
    assert!(
        backlog.retained_bytes_deficit > 0,
        "still over budget, so the two cases differ only in whether cleanup helps"
    );
    assert!(backlog.clears_retained_bytes_budget);
}

#[test]
fn status_carries_the_same_budget_verdict_the_plan_does() {
    let (tmp, broker) = fixture();
    let (mut broker, _, _, _, _) = two_candidates(&tmp, broker);
    write_policy(tmp.path(), 1);

    let retention = broker.status(now_ms()).unwrap().cleanup_retention;
    assert!(retention.over_retained_bytes_budget);
    assert_eq!(
        retention.retained_bytes_deficit,
        retention.estimated_retained_bytes - 1
    );
    assert!(
        !retention.clears_retained_bytes_budget,
        "status must not report a budget as satisfiable when reclamation cannot satisfy it"
    );
}

#[test]
fn an_unset_budget_is_off_rather_than_permanently_exceeded() {
    let (tmp, broker) = fixture();
    let (mut broker, _, _, _, _) = two_candidates(&tmp, broker);
    write_policy(tmp.path(), 0);

    let plan = broker.gc_plan().unwrap();
    assert_eq!(plan.retained_bytes_deficit, 0);
    assert!(plan.clears_retained_bytes_budget);
    assert_eq!(plan.reclaim_order, ReclaimOrder::OldestFirst);

    let retention = broker.status(now_ms()).unwrap().cleanup_retention;
    assert!(!retention.over_retained_bytes_budget);
    assert_eq!(retention.retained_bytes_deficit, 0);
}

/// Build caches record idleness rather than a closing time, so the plan has to
/// convert one into the other to rank them by the same rule as everything
/// else. If it did not, this list would keep its old session-id order while
/// the worktree list moved, and a bounded sweep would spend its deadline on
/// whichever cache happened to be created first.
#[test]
fn build_caches_follow_the_same_rule_the_worktrees_do() {
    let (tmp, broker) = fixture();
    let (mut broker, _, _, blocked_old, blocked_new) = two_candidates(&tmp, broker);

    write_policy(tmp.path(), 0);
    let relaxed = broker.gc_plan().unwrap();
    let relaxed_order: Vec<i64> = relaxed.artifacts.iter().map(|a| a.session_id).collect();
    assert_eq!(
        relaxed_order,
        vec![blocked_old, blocked_new],
        "within budget the longest-idle cache goes first"
    );

    write_policy(tmp.path(), 1);
    let pressed = broker.gc_plan().unwrap();
    let pressed_order: Vec<i64> = pressed.artifacts.iter().map(|a| a.session_id).collect();
    assert_eq!(
        pressed_order,
        vec![blocked_new, blocked_old],
        "over budget the largest cache goes first, idle days notwithstanding"
    );
}
