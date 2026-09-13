//! The routine check and the expensive audit must not be the same code (#176).
//!
//! Sizing a directory has no shortcut, so every byte figure costs a full
//! recursive walk. `broker status` is the mandated first step of every
//! session and reached that walk through the same `cleanup_plan` as
//! `gc plan --json`, which is why the routine check took five minutes on the
//! machine that motivated the issue.
//!
//! What is exercised here is the split and its honesty: the cheap path never
//! walks, it says how much it did not measure, and a floor under the budget
//! reports as undecided rather than as a pass. A cheap path that quietly
//! reported its floor as a total would be the original bug wearing a
//! different number.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, BudgetVerdict, FinishOptions};

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

/// `artifact_sweep_budget_ms = 0` disables the autonomous startup sweep, which
/// would otherwise reclaim fixture content before a test could measure it.
fn write_policy(main_root: &Path, retained_bytes_budget: u64, routine_size_budget_ms: u64) {
    std::fs::create_dir_all(main_root.join(".aethyme")).unwrap();
    std::fs::write(
        main_root.join(".aethyme/broker.toml"),
        format!(
            "[retention]\nclosed_worktrees_days = 1\nartifact_reclaim_days = 1\nartifact_sweep_budget_ms = 0\nstartup_budget_ms = 5\nretained_bytes_budget = {retained_bytes_budget}\nroutine_size_budget_ms = {routine_size_budget_ms}\nsize_record_ttl_hours = 24\n"
        ),
    )
    .unwrap();
}

fn fixture(retained_bytes_budget: u64, routine_size_budget_ms: u64) -> (tempfile::TempDir, Broker) {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    write_policy(tmp.path(), retained_bytes_budget, routine_size_budget_ms);
    let broker = Broker::open(tmp.path()).unwrap();
    (tmp, broker)
}

/// Deliver a session carrying `payload_bytes`, then close it keeping the
/// worktree, so it stays on disk as a retained candidate with real bytes.
fn deliver(broker: &mut Broker, task: &str, payload_bytes: usize) -> i64 {
    let session = broker.start_worktree(task, None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    // The payload size doubles as its content, so two sessions must never ask
    // for the same one: the second commit would be empty and git would refuse.
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

fn record_count(main_root: &Path) -> usize {
    let path = main_root.join(".aethyme/worktree-sizes.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    parsed["records"].as_object().map_or(0, |map| map.len())
}

/// The claim that makes the split worth having: with warming switched off, a
/// routine check produces a plan without opening a single directory.
///
/// Asserted through the numbers rather than a timer, because a fixture
/// worktree is small enough that walking it would also be fast. Reporting
/// zero bytes for a worktree that demonstrably holds a megabyte is only
/// possible if nothing walked it.
#[test]
fn a_routine_check_reports_only_what_was_already_measured() {
    let (tmp, mut broker) = fixture(1_024, 0);
    deliver(&mut broker, "retained delivery", 1_024 * 1_024);

    let plan = broker.cleanup_plan_recorded().unwrap();
    assert_eq!(plan.worktrees.len(), 1, "the candidate is still listed");
    assert_eq!(
        plan.estimated_retained_bytes, 0,
        "an unmeasured worktree contributes no bytes"
    );
    assert_eq!(plan.unmeasured_worktree_count, 1);
    assert_eq!(plan.sizes_measured_at_ms, None);
    assert_eq!(record_count(tmp.path()), 0, "warming is disabled");
}

/// A floor under the budget answers nothing, and must not be allowed to read
/// as a pass. This is the failure mode the one-sided verdict exists for: the
/// bytes the floor skipped are exactly the ones that would have decided it.
#[test]
fn an_unmeasured_total_under_the_budget_decides_nothing() {
    let (_tmp, mut broker) = fixture(1_024 * 1_024 * 1_024, 0);
    deliver(&mut broker, "retained delivery", 1_024 * 1_024);

    let plan = broker.gc_plan_recorded().unwrap();
    assert_eq!(plan.budget_verdict, BudgetVerdict::Unknown);
    assert_eq!(
        plan.retained_bytes_deficit, 0,
        "a floor below the budget names no deficit"
    );
    // `doctor`, `certify` and the verify loop all read this flag. Undecided
    // must not raise it, or every unmeasured install reports a breach.
    let health = broker.gc_health().unwrap();
    assert_eq!(health.budget_verdict, BudgetVerdict::Unknown);
    assert!(!health.over_retained_bytes_budget);
    assert_eq!(health.unmeasured_directory_count, 1);
}

/// The other side of the same verdict: a floor that already exceeds the budget
/// proves the breach, because the bytes it skipped could only add to it.
#[test]
fn a_floor_over_the_budget_is_conclusive() {
    let (_tmp, mut broker) = fixture(1_024, 0);
    deliver(&mut broker, "retained delivery", 1_024 * 1_024);
    // Measure once so the floor has something in it, then ask cheaply.
    broker.gc_plan().unwrap();

    let plan = broker.gc_plan_recorded().unwrap();
    assert_eq!(plan.budget_verdict, BudgetVerdict::Over);
    assert!(plan.retained_bytes_deficit > 0);
    let health = broker.gc_health().unwrap();
    assert!(
        health.over_retained_bytes_budget,
        "a breach proven by a floor is still a breach"
    );
}

/// The expensive walk is what feeds the cheap path. After one `gc plan`, a
/// routine check reports the same bytes without walking anything.
#[test]
fn the_expensive_walk_leaves_records_the_routine_check_reads() {
    let (tmp, mut broker) = fixture(1_024, 0);
    deliver(&mut broker, "retained delivery", 1_024 * 1_024);

    let measured = broker.gc_plan().unwrap();
    assert!(measured.estimated_retained_bytes >= 1_024 * 1_024);
    assert_eq!(measured.unmeasured_directory_count, 0);
    assert_eq!(record_count(tmp.path()), 1);

    let recorded = broker.cleanup_plan_recorded().unwrap();
    assert_eq!(
        recorded.estimated_retained_bytes, measured.estimated_retained_bytes,
        "the routine check reports the recorded figure"
    );
    assert_eq!(recorded.unmeasured_worktree_count, 0);
    assert!(recorded.sizes_measured_at_ms.is_some());
}

/// Warming is what keeps the records from depending on somebody remembering to
/// run the audit. Each routine check spends its small budget on one directory
/// nobody has sized, so the gap closes on its own.
#[test]
fn a_routine_check_warms_one_record_at_a_time() {
    let (tmp, mut broker) = fixture(1_024, 200);
    deliver(&mut broker, "first delivery", 1_024);
    deliver(&mut broker, "second delivery", 2_048);
    // Closing a session measures whatever is already retained, so the fixture
    // arrives with a record or two. Discarding them is not a contrivance: a
    // fresh checkout, a cleared `.aethyme`, or a records file from an older
    // schema all reach the cheap path with nothing measured, and that is the
    // state warming exists to climb out of.
    std::fs::remove_file(tmp.path().join(".aethyme/worktree-sizes.json")).ok();
    assert_eq!(record_count(tmp.path()), 0);

    let first = broker.cleanup_plan_recorded().unwrap();
    assert_eq!(
        first.unmeasured_worktree_count, 2,
        "the pass reports the gap it found, not the one it leaves"
    );
    assert_eq!(record_count(tmp.path()), 1);

    let second = broker.cleanup_plan_recorded().unwrap();
    assert_eq!(second.unmeasured_worktree_count, 1);
    assert_eq!(record_count(tmp.path()), 2);

    let third = broker.cleanup_plan_recorded().unwrap();
    assert_eq!(third.unmeasured_worktree_count, 0);
    assert_eq!(
        record_count(tmp.path()),
        2,
        "warming stops once every path has a record inside the TTL"
    );
    assert!(third.estimated_retained_bytes > 0);
}

/// A plan that does not know how big things are must not be able to authorize
/// removing them. The digest is the authorization, so the cheap path carries
/// none and `gc apply` has nothing to accept.
#[test]
fn a_routine_plan_cannot_authorize_removal() {
    let (_tmp, mut broker) = fixture(1_024, 0);
    deliver(&mut broker, "retained delivery", 1_024 * 1_024);

    let recorded = broker.gc_plan_recorded().unwrap();
    assert!(recorded.digest.is_empty(), "no digest, no authorization");
    assert!(
        broker.gc_apply(&recorded.digest).is_err(),
        "an empty confirmation is not a confirmation"
    );

    let measured = broker.gc_plan().unwrap();
    assert!(
        !measured.digest.is_empty(),
        "the expensive walk still authorizes"
    );
}

/// A confirmation has to survive being re-derived. `cleanup_cleaned_worktrees`
/// re-plans and compares digests before removing anything, so any field that
/// changes between two identical plans makes every confirmation unusable. The
/// measurement timestamp is exactly such a field, which is why it is written
/// after the digest rather than into it.
#[test]
fn two_identical_audits_agree_on_the_digest() {
    let (_tmp, mut broker) = fixture(1_024, 0);
    deliver(&mut broker, "retained delivery", 1_024);

    let first = broker.cleanup_plan().unwrap();
    let second = broker.cleanup_plan().unwrap();
    assert!(!first.digest.is_empty());
    assert_eq!(first.digest, second.digest);
    assert!(
        second.sizes_measured_at_ms.is_some(),
        "the timestamp is still reported, just not authorized against"
    );
}
