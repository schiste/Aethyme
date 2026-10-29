//! #588: deterministic automatic removal of disposable session checkouts.
//!
//! Every fixture is a clone of a bare `origin`, with a publisher clone
//! standing in for the forge, so "contained in the remote default branch" is
//! a real remote-tracking ref that local branches cannot satisfy.

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{AutoCleanupReport, Broker};

mod common;

fn git_out(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
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

fn git(dir: &Path, args: &[&str]) {
    git_out(dir, args);
}

fn commit(dir: &Path, file: &str, content: &str, message: &str) -> String {
    std::fs::write(dir.join(file), content).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-qm", message]);
    git_out(dir, &["rev-parse", "HEAD"])
}

struct Fixture {
    _tmp: tempfile::TempDir,
    publisher: PathBuf,
    repo: PathBuf,
}

/// A bare origin, a publisher clone and the repository under test.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin.git");
    let publisher = tmp.path().join("publisher");
    let repo = tmp.path().join("repo");
    git(
        tmp.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(
        tmp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "publisher"],
    );
    std::fs::write(
        publisher.join(".gitignore"),
        "/.aethyme/\ntarget/\nnode_modules/\n.env\n",
    )
    .unwrap();
    std::fs::write(publisher.join("shared.txt"), "one\n").unwrap();
    git(&publisher, &["add", "-A"]);
    git(&publisher, &["commit", "-qm", "init"]);
    git(&publisher, &["push", "-q", "origin", "main"]);
    git(
        tmp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "repo"],
    );
    // Most cases exercise immediate cleanup. Grace behavior is covered
    // explicitly below rather than depending on fixture wall-clock age.
    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    std::fs::write(
        repo.join(".aethyme/broker.toml"),
        "[retention]\nclosed_worktree_grace_hours = 0\n",
    )
    .unwrap();
    Fixture {
        _tmp: tmp,
        publisher,
        repo,
    }
}

/// A closed session whose one commit reached origin/main by a merge commit.
fn merged_closed_session(fx: &Fixture, broker: &mut Broker, close: bool) -> (i64, PathBuf, String) {
    let session = broker.start_worktree("merged work", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    // Content unique to the session, so a second one still has a change.
    let content = format!("work {}\n", session.id);
    let head = commit(&worktree, "feature.txt", &content, "feature");
    // The forge merges the branch with a merge commit.
    git(
        &worktree,
        &[
            "push",
            "-q",
            "origin",
            &format!("HEAD:refs/heads/{}", session.branch),
        ],
    );
    git(&fx.publisher, &["fetch", "-q", "origin"]);
    git(
        &fx.publisher,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "merge",
            &format!("origin/{}", session.branch),
        ],
    );
    git(&fx.publisher, &["push", "-q", "origin", "main"]);
    git(&fx.repo, &["fetch", "-q", "origin"]);
    if close {
        broker.close(session.id).unwrap();
    }
    (session.id, worktree, head)
}

fn run(broker: &mut Broker) -> AutoCleanupReport {
    broker
        .auto_cleanup_now(None)
        .unwrap()
        .expect("no other GC holds the lock")
}

fn kept_reason<'a>(report: &'a AutoCleanupReport, worktree: &Path) -> &'a str {
    let canonical = std::fs::canonicalize(worktree)
        .unwrap_or_else(|_| worktree.to_path_buf())
        .to_string_lossy()
        .into_owned();
    report
        .kept
        .iter()
        .find(|kept| kept.worktree == canonical)
        .map(|kept| kept.reason.as_str())
        .unwrap_or_else(|| panic!("{} not kept: {report:#?}", worktree.display()))
}

fn persisted_spawn_witness_count(broker: &mut Broker, session_id: i64) -> i64 {
    rusqlite::Connection::open_with_flags(
        broker.store().db_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row(
        "SELECT COUNT(*) FROM spawned_worktree_instances WHERE session_id = ?1",
        [session_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn persisted_spawn_witness_root_generation(broker: &mut Broker, session_id: i64) -> String {
    let connection = rusqlite::Connection::open_with_flags(
        broker.store().db_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let witness: String = connection
        .query_row(
            "SELECT witness_json FROM spawned_worktree_instances WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&witness).unwrap();
    value
        .get("root_generation_sha256")
        .and_then(serde_json::Value::as_str)
        .expect("spawn must persist the root generation digest")
        .to_owned()
}

fn assert_alias_inventory_barrier(report: &AutoCleanupReport, deferred: usize) {
    assert!(report.removed.is_empty(), "{report:#?}");
    assert_eq!(report.deferred, deferred, "{report:#?}");
    let inventory = report
        .kept
        .iter()
        .find(|kept| kept.worktree == "<session checkout inventory>")
        .expect("stale alias proof must keep the whole cleanup batch");
    assert!(
        inventory.reason.contains("registered checkout paths"),
        "{inventory:#?}"
    );
}

/// Match repositories that explicitly keep pnpm's store inside each checkout.
fn pnpm_store_fixture(ignored: bool) -> Fixture {
    let fx = fixture();
    if ignored {
        let ignore = fx.repo.join(".gitignore");
        let mut text = std::fs::read_to_string(&ignore).unwrap();
        text.push_str(".pnpm-store/\n");
        std::fs::write(ignore, text).unwrap();
        git(&fx.repo, &["add", ".gitignore"]);
    }
    commit(
        &fx.repo,
        ".npmrc",
        "store-dir=.pnpm-store\n",
        "local pnpm store",
    );
    git(&fx.repo, &["push", "-q", "origin", "main"]);
    fx
}

#[cfg(unix)]
#[test]
fn an_ignored_pnpm_store_does_not_keep_a_merged_closed_checkout() {
    let fx = pnpm_store_fixture(true);
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (session_id, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::create_dir_all(worktree.join(".pnpm-store/v10/files/00")).unwrap();
    std::fs::write(worktree.join(".pnpm-store/v10/files/00/content"), "package").unwrap();
    let report = run(&mut broker);
    assert!(
        report
            .removed
            .iter()
            .any(|entry| entry.sessions.contains(&session_id)),
        "{report:#?}"
    );
    assert!(
        !worktree.exists(),
        "the safe ignored package store must not retain the checkout"
    );
}

#[cfg(unix)]
#[test]
fn sensitive_fixture_content_in_an_ignored_pnpm_store_is_discarded() {
    let fx = pnpm_store_fixture(true);
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::create_dir_all(worktree.join(".pnpm-store")).unwrap();
    std::fs::write(worktree.join(".pnpm-store/.env"), "private").unwrap();
    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    assert!(!worktree.exists());
}

#[test]
fn an_untracked_pnpm_store_keeps_the_checkout() {
    let fx = pnpm_store_fixture(false);
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::create_dir_all(worktree.join(".pnpm-store")).unwrap();
    let file = worktree.join(".pnpm-store/content");
    std::fs::write(&file, "untracked").unwrap();
    let report = run(&mut broker);
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(
        kept_reason(&report, &worktree).contains("untracked"),
        "{report:#?}"
    );
    assert_eq!(std::fs::read_to_string(file).unwrap(), "untracked");
}

#[cfg(unix)]
#[test]
fn a_merged_closed_checkout_is_removed_by_ancestry_and_journaled() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (session_id, worktree, head) = merged_closed_session(&fx, &mut broker, true);
    // Regenerable output and Finder metadata do not keep it.
    std::fs::create_dir_all(worktree.join("target/debug")).unwrap();
    std::fs::write(worktree.join("target/debug/app"), "bin").unwrap();
    // Cargo writes this into every target directory it owns.
    std::fs::write(
        worktree.join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    std::fs::write(worktree.join(".DS_Store"), "finder").unwrap();

    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    let removed = &report.removed[0];
    assert_eq!(removed.proof, "ancestry");
    assert_eq!(removed.head, head);
    assert_eq!(removed.contained_in, "refs/remotes/origin/main");
    assert!(!worktree.exists());
    let session = broker.store().session(session_id).unwrap();
    assert_eq!(
        session.cleanup_state,
        aethyme_broker::SessionCleanupState::Cleaned
    );
    assert!(
        git_out(&fx.repo, &["branch", "--list", &session.branch]).is_empty(),
        "the proved session branch is deleted"
    );
    let events = broker
        .store()
        .events_after(0, 10_000)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "broker.cleanup.auto_removed")
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    // `status` and `gc plan` both show it.
    let plan = broker.gc_plan().unwrap();
    assert_eq!(plan.auto_cleanup.as_ref().unwrap().removed.len(), 1);
}

#[cfg(unix)]
#[test]
fn a_squash_landing_is_removed_through_the_deep_proof() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let session = broker.start_worktree("squashed", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    commit(&worktree, "feature.txt", "work\n", "feature");
    broker.close(session.id).unwrap();
    git(&fx.publisher, &["pull", "-q", "--ff-only"]);
    std::fs::copy(
        worktree.join("feature.txt"),
        fx.publisher.join("feature.txt"),
    )
    .unwrap();
    git(&fx.publisher, &["add", "feature.txt"]);
    git(&fx.publisher, &["commit", "-qm", "squashed"]);
    git(&fx.publisher, &["push", "-q", "origin", "main"]);
    git(&fx.repo, &["fetch", "-q", "origin"]);

    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    assert_eq!(report.removed[0].proof, "content");
    assert!(!worktree.exists());
}

#[test]
fn a_deep_proof_the_budget_cannot_afford_is_deferred_not_removed() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let session = broker.start_worktree("squashed", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    commit(&worktree, "feature.txt", "work\n", "feature");
    broker.close(session.id).unwrap();
    git(&fx.publisher, &["pull", "-q", "--ff-only"]);
    std::fs::copy(
        worktree.join("feature.txt"),
        fx.publisher.join("feature.txt"),
    )
    .unwrap();
    git(&fx.publisher, &["add", "feature.txt"]);
    git(&fx.publisher, &["commit", "-qm", "squashed"]);
    git(&fx.publisher, &["push", "-q", "origin", "main"]);
    git(&fx.repo, &["fetch", "-q", "origin"]);

    let report = broker
        .auto_cleanup_now(Some(std::time::Instant::now()))
        .unwrap()
        .unwrap();
    assert!(report.removed.is_empty(), "{report:#?}");
    // The expired budget defers both retirement and checkout inspection.
    assert_eq!(report.deferred, 2, "{report:#?}");
    assert_eq!(report.sessions_retired, 0);
    assert!(matches!(
        report.retirement_deferred_reasons.as_slice(),
        [
            aethyme_broker::SessionRetirementDeferredReason::DeadlineExceeded {
                after_session_id: 0
            }
        ]
    ));
    assert!(worktree.exists());
}

#[test]
fn deadline_before_first_candidate_does_not_advance_cleanup_cursor() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let cursor_before = broker.store().meta_get("auto_cleanup.cursor").unwrap();

    let report = broker
        .auto_cleanup_now(Some(std::time::Instant::now()))
        .unwrap()
        .unwrap();

    assert!(report.removed.is_empty(), "{report:#?}");
    // The expired budget defers both retirement and checkout inspection.
    assert_eq!(report.deferred, 2, "{report:#?}");
    assert_eq!(report.sessions_retired, 0);
    assert!(matches!(
        report.retirement_deferred_reasons.as_slice(),
        [
            aethyme_broker::SessionRetirementDeferredReason::DeadlineExceeded {
                after_session_id: 0
            }
        ]
    ));
    assert_eq!(
        broker.store().meta_get("auto_cleanup.cursor").unwrap(),
        cursor_before,
        "a deadline before the first candidate must leave scheduling state untouched"
    );
    assert!(worktree.exists());
}

#[test]
fn an_open_session_keeps_its_checkout() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, false);
    let report = run(&mut broker);
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(worktree.exists());
}

#[test]
fn a_tracked_change_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::write(worktree.join("feature.txt"), "edited\n").unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("uncommitted or untracked"));
    assert!(worktree.exists());
}

#[test]
fn an_inspected_kept_checkout_checkpoints_the_cursor() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (session_id, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::write(worktree.join("notes.md"), "mine\n").unwrap();

    let report = run(&mut broker);

    assert!(kept_reason(&report, &worktree).contains("notes.md"));
    let cursor = broker
        .store()
        .meta_get("auto_cleanup.cursor")
        .unwrap()
        .expect("an inspected candidate checkpoints the cursor");
    let cursor: serde_json::Value = serde_json::from_str(&cursor).unwrap();
    assert_eq!(cursor["version"], 3);
    assert_eq!(cursor["after_session_id"], session_id);
    assert_eq!(cursor["high_water_session_id"], session_id);
    assert!(
        cursor["cohort_generation"]
            .as_i64()
            .is_some_and(|value| value > 0)
    );
    assert!(worktree.exists());
}

#[test]
fn an_untracked_file_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::write(worktree.join("notes.md"), "mine\n").unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("notes.md"));
    assert!(worktree.exists());
}

#[test]
fn standalone_ignored_root_files_still_keep_the_checkout() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::write(worktree.join(".env"), "SECRET=1\n").unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("requires explicit cleanup"));
    assert!(worktree.exists());
}

#[test]
fn a_stash_made_on_the_branch_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::write(worktree.join("feature.txt"), "stashed\n").unwrap();
    git(&worktree, &["stash", "-q"]);
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("stash"));
    assert!(worktree.exists());
}

#[test]
fn a_rebase_in_progress_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let git_dir = git_out(&worktree, &["rev-parse", "--absolute-git-dir"]);
    std::fs::create_dir_all(Path::new(&git_dir).join("rebase-merge")).unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("rebase"));
    assert!(worktree.exists());
}

#[test]
fn a_commit_not_on_the_remote_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let session = broker.start_worktree("unpushed", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    commit(&worktree, "feature.txt", "work\n", "feature");
    broker.close(session.id).unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("not contained"));
    assert!(worktree.exists());
}

#[test]
fn a_commit_only_in_a_local_branch_is_not_contained() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let session = broker.start_worktree("local only", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    let head = commit(&worktree, "feature.txt", "work\n", "feature");
    broker.close(session.id).unwrap();
    // Local main and the integration branch hold it; the remote does not.
    git(&fx.repo, &["branch", "-f", "aethyme/integration", &head]);
    git(&fx.repo, &["merge", "-q", "--ff-only", &head]);
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("not contained"));
    assert!(worktree.exists());
}

#[test]
fn no_fetched_remote_default_keeps_everything() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    git(
        &fx.repo,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("no fetched remote default branch"));
    assert!(worktree.exists());
}

#[test]
fn a_running_gate_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let run_dir = fx.repo.join(".aethyme/run/gates");
    std::fs::create_dir_all(&run_dir).unwrap();
    let pid = std::process::id();
    std::fs::write(
        run_dir.join("cargo-test.pid"),
        format!("{pid} deadbeef {pid}\n"),
    )
    .unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("gate"));
    assert!(worktree.exists());
}

#[test]
fn a_process_in_the_checkout_keeps_it() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let mut child = Command::new("sleep")
        .arg("30")
        .current_dir(&worktree)
        .spawn()
        .unwrap();
    #[cfg(unix)]
    let child_pid = child.id();
    let report = run(&mut broker);
    let _ = child.kill();
    let _ = child.wait();
    let reason = kept_reason(&report, &worktree);
    #[cfg(unix)]
    {
        let expected_prefix = format!("process {child_pid} (\"sleep\") has \"");
        let worktree_path = worktree.to_string_lossy();
        assert!(
            reason.starts_with(expected_prefix.as_str())
                && reason.contains(worktree_path.as_ref())
                && reason.ends_with(" open"),
            "{report:#?}"
        );
    }
    #[cfg(not(unix))]
    assert!(
        reason.contains("spawn-time worktree instance identity is unavailable"),
        "{report:#?}"
    );
    assert!(worktree.exists());
}

#[test]
fn closing_releases_leases_so_none_can_hold_a_closed_checkout() {
    // `close` deletes the session's leases in the same transaction, so a
    // closed checkout never has a holding lease of its own. The lease proof
    // stays as a guard; this pins the invariant it relies on.
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (session_id, worktree, _) = merged_closed_session(&fx, &mut broker, false);
    broker.claim_lease(session_id, "feature.txt", None).unwrap();
    broker.close(session_id).unwrap();
    assert!(
        broker
            .store()
            .session_leases(session_id)
            .unwrap()
            .is_empty()
    );
    let report = run(&mut broker);
    #[cfg(unix)]
    {
        assert_eq!(report.removed.len(), 1, "{report:#?}");
        assert!(!worktree.exists());
    }
    #[cfg(not(unix))]
    {
        assert!(report.removed.is_empty(), "{report:#?}");
        assert!(
            kept_reason(&report, &worktree)
                .contains("spawn-time worktree instance identity is unavailable"),
            "{report:#?}"
        );
        assert!(worktree.exists());
    }
}

#[cfg(unix)]
#[test]
fn retained_unknown_lease_on_closed_candidate_is_kept_amid_unrelated_leases() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (session_id, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let unrelated = broker
        .start_worktree("unrelated lease history", None)
        .unwrap();

    // Simulate a retained post-close row and remove its holder binding
    // so liveness must classify it as unknown, which keeps the checkout.
    let db_path = broker.store().db_path().to_path_buf();
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute(
            "DELETE FROM events WHERE session_id = ?1 AND kind = ?2",
            rusqlite::params![session_id, "session.holder_bound"],
        )
        .unwrap();
    broker
        .store()
        .claim_lease(session_id, "feature.txt", None)
        .unwrap();

    // Unrelated active rows must not hide or replace the candidate's lease
    // evidence when cleanup scopes its query to the closed checkout cohort.
    for index in 0..128 {
        broker
            .store()
            .claim_lease(unrelated.id, &format!("unrelated-{index}"), None)
            .unwrap();
    }

    let report = run(&mut broker);

    assert!(
        kept_reason(&report, &worktree).contains("lease on feature.txt is unknown and still holds"),
        "{report:#?}"
    );
    assert!(worktree.exists());
}

#[test]
fn a_live_session_that_adopted_the_closed_checkout_keeps_it() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let adopted = broker.adopt(&worktree, Some("picked up again")).unwrap();
    let report = run(&mut broker);
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(
        kept_reason(&report, &worktree).contains(&format!("session {} is still open", adopted.id)),
        "{report:#?}"
    );
    assert!(worktree.exists());
}

#[cfg(not(unix))]
#[test]
fn unsupported_platform_spawn_identity_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (spawned_id, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    assert_eq!(persisted_spawn_witness_count(&mut broker, spawned_id), 0);

    let report = run(&mut broker);

    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(
        kept_reason(&report, &worktree)
            .contains("spawn-time worktree instance identity is unavailable"),
        "{report:#?}"
    );
    assert!(worktree.exists());
}

#[cfg(unix)]
#[test]
fn a_retained_spawned_session_owns_a_closed_re_adoption() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (spawned_id, worktree, head) = merged_closed_session(&fx, &mut broker, true);
    assert_eq!(
        persisted_spawn_witness_count(&mut broker, spawned_id),
        1,
        "the spawned session stores its identity in the registration path"
    );
    let adopted = broker.adopt(&worktree, Some("picked up again")).unwrap();
    assert_eq!(
        persisted_spawn_witness_count(&mut broker, adopted.id),
        0,
        "adoption does not create spawn authority"
    );
    broker.close(adopted.id).unwrap();

    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    let removed = &report.removed[0];
    assert_eq!(removed.head, head);
    assert!(removed.sessions.contains(&spawned_id));
    assert!(removed.sessions.contains(&adopted.id));
    assert!(!worktree.exists());
}

#[cfg(unix)]
#[test]
fn detached_agent_spawn_registration_persists_instance_witness() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();

    let report = broker.start_agent_report("witness", "true", None).unwrap();

    assert_eq!(
        persisted_spawn_witness_count(&mut broker, report.session.id),
        1
    );
}

#[test]
fn a_cleaned_spawn_record_does_not_authorize_a_closed_adoption() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (spawned_id, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    broker
        .store()
        .set_session_status(spawned_id, aethyme_broker::SessionStatus::Cleaned, None)
        .unwrap();
    let adopted = broker
        .adopt(&worktree, Some("adopted after prior cleanup"))
        .unwrap();
    broker.close(adopted.id).unwrap();

    let report = run(&mut broker);
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(kept_reason(&report, &worktree).contains("spawn-time worktree instance witness"));
    assert!(worktree.exists());
}

#[test]
fn a_spawned_session_without_a_persisted_witness_is_not_backfilled() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (spawned_id, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let db_path = broker.store().db_path().to_path_buf();
    let legacy_writer = rusqlite::Connection::open(db_path).unwrap();
    legacy_writer
        .execute(
            "DELETE FROM spawned_worktree_instances WHERE session_id = ?1",
            [spawned_id],
        )
        .unwrap();
    drop(legacy_writer);

    let report = run(&mut broker);

    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(
        kept_reason(&report, &worktree)
            .contains("closed checkout cohort or ownership witness changed")
    );
    assert_eq!(
        persisted_spawn_witness_count(&mut broker, spawned_id),
        0,
        "cleanup must not manufacture missing spawn-time evidence"
    );
    assert!(
        worktree.exists(),
        "missing historical proof must not be inferred"
    );
}

#[test]
fn a_closed_re_adoption_after_selection_keeps_the_checkout_for_that_pass() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (spawned_id, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let plan = broker.auto_cleanup_plan_now(None).unwrap().unwrap();
    #[cfg(unix)]
    assert_eq!(plan.selected_worktrees().len(), 1);
    #[cfg(not(unix))]
    assert!(plan.selected_worktrees().is_empty());

    let adopted = broker
        .adopt(&worktree, Some("adopted after selection"))
        .unwrap();
    broker.close(adopted.id).unwrap();
    let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();

    assert!(report.removed.is_empty(), "{report:#?}");
    #[cfg(unix)]
    {
        let canonical = std::fs::canonicalize(&worktree).unwrap();
        let inventory_deferred = report.deferred == 1 && report.kept.is_empty();
        let cohort_kept = report.deferred == 0
            && report.kept.iter().any(|kept| {
                kept.worktree == canonical.to_string_lossy().into_owned()
                    && kept.reason.contains("closed session set changed")
            });
        assert!(inventory_deferred || cohort_kept, "{report:#?}");
    }
    #[cfg(not(unix))]
    assert!(
        kept_reason(&report, &worktree)
            .contains("spawn-time worktree instance identity is unavailable"),
        "{report:#?}"
    );
    assert!(worktree.exists());
    assert_eq!(
        broker.store().session(spawned_id).unwrap().cleanup_state,
        aethyme_broker::SessionCleanupState::Closed
    );
    assert_eq!(
        broker.store().session(adopted.id).unwrap().cleanup_state,
        aethyme_broker::SessionCleanupState::Closed
    );
}

#[cfg(unix)]
#[test]
fn a_retained_spawn_record_does_not_own_a_foreign_checkout_replacement() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (spawned_id, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let plan = broker.auto_cleanup_plan_now(None).unwrap().unwrap();
    assert_eq!(plan.selected_worktrees().len(), 1);
    let spawn_generation = persisted_spawn_witness_root_generation(&mut broker, spawned_id);
    let worktree_text = worktree.to_string_lossy().into_owned();
    git(&fx.repo, &["worktree", "remove", "--force", &worktree_text]);

    // Recreate the same pathname as a worktree of a different Git repository.
    // The broker-root marker still names this parent, so cleanup must also
    // compare the replacement checkout's Git common directory.
    let foreign_repo = fx._tmp.path().join("foreign-repo");
    let foreign_repo_name = foreign_repo.file_name().unwrap().to_string_lossy();
    let origin = fx
        ._tmp
        .path()
        .join("origin.git")
        .to_string_lossy()
        .into_owned();
    git(
        fx._tmp.path(),
        &["clone", "-q", &origin, foreign_repo_name.as_ref()],
    );
    git(
        &foreign_repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &worktree_text,
            "origin/main",
        ],
    );
    let main_common_dir = git_out(
        &fx.repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let foreign_common_dir = git_out(
        &worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    assert_ne!(main_common_dir, foreign_common_dir);

    let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();

    assert_alias_inventory_barrier(&report, 1);
    assert_eq!(
        persisted_spawn_witness_root_generation(&mut broker, spawned_id),
        spawn_generation,
        "stale apply must not bind the foreign replacement to the original spawn witness"
    );
    let refreshed = run(&mut broker);
    assert!(refreshed.removed.is_empty(), "{refreshed:#?}");
    assert!(kept_reason(&refreshed, &worktree).contains("not a broker-owned worktree"));
    assert!(worktree.join("shared.txt").exists());
    assert!(worktree.exists());
    assert_eq!(
        broker.store().session(spawned_id).unwrap().cleanup_state,
        aethyme_broker::SessionCleanupState::Closed,
        "the old spawn record remains available but cannot authorize a foreign replacement"
    );
}

#[cfg(unix)]
#[test]
fn a_retained_spawn_record_does_not_own_a_same_repo_checkout_replacement() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (spawned_id, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let plan = broker.auto_cleanup_plan_now(None).unwrap().unwrap();
    assert_eq!(plan.selected_worktrees().len(), 1);
    let worktree_text = worktree.to_string_lossy().into_owned();
    let branch = broker.store().session(spawned_id).unwrap().branch;
    let spawn_generation = persisted_spawn_witness_root_generation(&mut broker, spawned_id);
    assert_eq!(spawn_generation.len(), 64);
    git(&fx.repo, &["worktree", "remove", "--force", &worktree_text]);
    git(
        &fx.repo,
        &["worktree", "add", "-q", &worktree_text, &branch],
    );

    let main_common_dir = git_out(
        &fx.repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let replacement_common_dir = git_out(
        &worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    assert_eq!(main_common_dir, replacement_common_dir);
    assert_eq!(git_out(&worktree, &["branch", "--show-current"]), branch);

    let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();

    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(
        kept_reason(&report, &worktree)
            .contains("closed checkout cohort or ownership witness changed")
    );
    assert_eq!(
        persisted_spawn_witness_root_generation(&mut broker, spawned_id),
        spawn_generation,
        "the retained witness must not be rebound to the replacement instance"
    );
    assert!(worktree.join("shared.txt").exists());
    assert!(worktree.exists());
    assert_eq!(
        broker.store().session(spawned_id).unwrap().cleanup_state,
        aethyme_broker::SessionCleanupState::Closed,
        "the retained spawn row must not authorize a replacement instance"
    );
}

#[test]
fn a_change_between_selection_and_removal_keeps_the_checkout() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let plan = broker.auto_cleanup_plan_now(None).unwrap().unwrap();
    #[cfg(unix)]
    assert_eq!(plan.selected_worktrees().len(), 1);
    #[cfg(not(unix))]
    assert!(plan.selected_worktrees().is_empty());
    std::fs::write(worktree.join("late.txt"), "written after selection\n").unwrap();
    let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();
    assert!(report.removed.is_empty(), "{report:#?}");
    #[cfg(unix)]
    assert!(kept_reason(&report, &worktree).contains("changed before removal"));
    #[cfg(not(unix))]
    assert!(
        kept_reason(&report, &worktree)
            .contains("spawn-time worktree instance identity is unavailable"),
        "{report:#?}"
    );
    assert!(worktree.exists());
}

#[test]
fn a_head_moved_after_selection_is_proved_again_not_reused() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let plan = broker.auto_cleanup_plan_now(None).unwrap().unwrap();
    #[cfg(unix)]
    assert_eq!(plan.selected_worktrees().len(), 1);
    #[cfg(not(unix))]
    assert!(plan.selected_worktrees().is_empty());
    // A commit no remote has: the selection's proof names the old HEAD and
    // must not vouch for this one.
    commit(&worktree, "late.txt", "committed after selection\n", "late");
    let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();
    assert!(report.removed.is_empty(), "{report:#?}");
    #[cfg(unix)]
    assert!(kept_reason(&report, &worktree).contains("changed before removal"));
    #[cfg(not(unix))]
    assert!(
        kept_reason(&report, &worktree)
            .contains("spawn-time worktree instance identity is unavailable"),
        "{report:#?}"
    );
    assert!(worktree.exists());
}

#[cfg(unix)]
#[test]
fn expired_selection_deadline_defers_until_alias_inventory_can_be_rechecked() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, first, _) = merged_closed_session(&fx, &mut broker, true);
    let (_, second, _) = merged_closed_session(&fx, &mut broker, true);
    // An already-expired deadline cannot complete the mandatory apply-time
    // filesystem alias audit, so none of the selected paths is authorized.
    // Injected, so a loaded runner cannot race it.
    let plan = broker
        .auto_cleanup_plan_now(None)
        .unwrap()
        .unwrap()
        .with_deadline(Some(std::time::Instant::now()));
    assert_eq!(plan.selected_worktrees().len(), 2);
    let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();
    assert_alias_inventory_barrier(&report, 2);
    assert!(first.exists() && second.exists());
    // A fresh unbounded pass can complete the audit and make progress.
    assert_eq!(run(&mut broker).removed.len(), 2);
    assert!(!first.exists() && !second.exists());
}

#[test]
fn auto_remove_false_disables_it_and_keep_pins_apply() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::create_dir_all(fx.repo.join(".aethyme")).unwrap();
    let config = fx.repo.join(".aethyme/config.toml");

    std::fs::write(&config, "[cleanup]\nauto_remove = false\n").unwrap();
    let report = run(&mut broker);
    assert!(!report.enabled);
    assert!(report.removed.is_empty());
    assert!(worktree.exists());

    let name = worktree.file_name().unwrap().to_string_lossy().into_owned();
    std::fs::write(&config, format!("[cleanup]\nkeep = [\"{name}\"]\n")).unwrap();
    let report = run(&mut broker);
    assert!(kept_reason(&report, &worktree).contains("pinned"));
    assert!(worktree.exists());
}

#[test]
fn the_primary_checkout_and_non_broker_worktrees_are_never_removed() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    // A worktree the broker did not create, adopted and closed.
    let foreign = fx.repo.parent().unwrap().join("foreign");
    git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            foreign.to_str().unwrap(),
            "-b",
            "foreign",
        ],
    );
    let adopted = broker.adopt(&foreign, Some("foreign")).unwrap();
    broker.close(adopted.id).unwrap();
    // The primary checkout itself, adopted and closed.
    let primary = broker.adopt(&fx.repo, Some("primary")).unwrap();
    broker.close(primary.id).unwrap();

    let report = run(&mut broker);
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(foreign.exists());
    assert!(fx.repo.exists());
    assert!(
        kept_reason(&report, &foreign).contains("no matching spawn-time worktree instance witness")
            || kept_reason(&report, &foreign).contains("not a broker-owned worktree")
            || kept_reason(&report, &foreign)
                .contains("spawn-time worktree instance identity is unavailable"),
        "{report:#?}"
    );
}

/// Ignore `pattern` in every checkout of the fixture's repository.
fn ignore(fx: &Fixture, pattern: &str) {
    let exclude = fx.repo.join(".git/info/exclude");
    let mut text = std::fs::read_to_string(&exclude).unwrap_or_default();
    text.push_str(pattern);
    text.push('\n');
    std::fs::write(exclude, text).unwrap();
}

#[cfg(unix)]
#[test]
fn ignored_directories_are_disposable_without_names_or_witnesses() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    ignore(&fx, "build/");
    ignore(&fx, "target-notes/");
    ignore(&fx, "saved-sessions/");
    std::fs::create_dir_all(worktree.join("build")).unwrap();
    std::fs::write(worktree.join("build/generate.py"), "print('mine')\n").unwrap();
    std::fs::create_dir_all(worktree.join("target-notes")).unwrap();
    std::fs::write(worktree.join("target-notes/plan.md"), "notes\n").unwrap();
    std::fs::create_dir_all(worktree.join("saved-sessions")).unwrap();
    std::fs::write(worktree.join("saved-sessions/session.db"), "private\n").unwrap();
    std::fs::create_dir_all(worktree.join("node_modules/pkg/.aethyme")).unwrap();
    std::fs::write(worktree.join("node_modules/.env.local"), "secret\n").unwrap();
    std::fs::write(
        worktree.join("node_modules/pkg/.aethyme/state.json"),
        "local session\n",
    )
    .unwrap();

    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    assert!(!worktree.exists());
}

#[cfg(unix)]
#[test]
fn an_escaping_symlink_inside_an_ignored_directory_is_unlinked_without_following() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("precious.txt"), "keep\n").unwrap();
    std::fs::create_dir_all(worktree.join("node_modules")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), worktree.join("node_modules/linked")).unwrap();
    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    assert!(!worktree.exists());
    assert!(elsewhere.path().join("precious.txt").exists());
}

#[cfg(unix)]
#[test]
fn a_known_root_symlink_is_unlinked_without_following_its_external_target() {
    let fx = fixture();
    ignore(&fx, "node_modules");
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("precious.txt"), "keep\n").unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), worktree.join("node_modules")).unwrap();

    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    assert!(!worktree.exists());
    assert!(elsewhere.path().join("precious.txt").exists());
}

#[cfg(unix)]
#[test]
fn strict_mode_keeps_ignored_files_and_directories_until_they_are_removed() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    ignore(&fx, ".DS_Store");
    ignore(&fx, "saved-sessions/");
    std::fs::write(
        fx.repo.join(".aethyme/config.toml"),
        "[cleanup]\nstrict = true\n",
    )
    .unwrap();

    let finder_metadata = worktree.join(".DS_Store");
    std::fs::write(&finder_metadata, "local\n").unwrap();
    let report = run(&mut broker);
    assert!(
        kept_reason(&report, &worktree).contains("strict cleanup"),
        "{report:#?}"
    );
    assert!(finder_metadata.exists());

    std::fs::remove_file(&finder_metadata).unwrap();
    let known_directory = worktree.join("node_modules");
    std::fs::create_dir_all(&known_directory).unwrap();
    std::fs::write(known_directory.join("package.json"), "{}\n").unwrap();
    let report = run(&mut broker);
    assert!(
        kept_reason(&report, &worktree).contains("strict cleanup"),
        "{report:#?}"
    );
    assert!(known_directory.exists());

    std::fs::remove_dir_all(&known_directory).unwrap();
    let unknown_directory = worktree.join("saved-sessions");
    std::fs::create_dir_all(&unknown_directory).unwrap();
    std::fs::write(unknown_directory.join("session.json"), "{}\n").unwrap();
    let report = run(&mut broker);
    assert!(
        kept_reason(&report, &worktree).contains("strict cleanup"),
        "{report:#?}"
    );
    assert!(unknown_directory.exists());

    std::fs::remove_dir_all(&unknown_directory).unwrap();
    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    assert!(!worktree.exists());
}

#[test]
fn explicit_non_force_cleanup_obeys_strict_mode() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (session_id, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::write(
        fx.repo.join(".aethyme/config.toml"),
        "[cleanup]\nstrict = true\n",
    )
    .unwrap();
    std::fs::create_dir_all(worktree.join("node_modules")).unwrap();
    std::fs::write(worktree.join("node_modules/.env"), "secret\n").unwrap();

    let error = broker.cleanup(session_id, false).unwrap_err();
    assert!(error.to_string().contains("strict cleanup"), "{error}");
    assert!(worktree.join("node_modules/.env").exists());
    assert!(worktree.exists());

    std::fs::remove_dir_all(worktree.join("node_modules")).unwrap();
    broker.cleanup(session_id, false).unwrap();
    assert!(!worktree.exists());
}

#[test]
fn a_regenerable_glob_that_could_match_a_secret_is_rejected() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::create_dir_all(fx.repo.join(".aethyme")).unwrap();
    std::fs::write(
        fx.repo.join(".aethyme/config.toml"),
        "[cleanup]\nregenerable = [\".env*\"]\n",
    )
    .unwrap();
    let report = run(&mut broker);
    assert!(!report.enabled, "{report:#?}");
    assert!(
        report
            .config_error
            .as_deref()
            .unwrap_or_default()
            .contains(".env")
    );
    assert!(report.removed.is_empty());
    assert!(worktree.exists());
}

#[test]
fn an_unreadable_checkout_status_keeps_it() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    // A corrupt index makes `git status` fail: not knowing is not clean.
    let git_dir = git_out(&worktree, &["rev-parse", "--absolute-git-dir"]);
    std::fs::write(Path::new(&git_dir).join("index"), "not an index").unwrap();
    let report = run(&mut broker);
    assert!(
        kept_reason(&report, &worktree).contains("could not be checked"),
        "{report:#?}"
    );
    assert!(worktree.exists());
}

#[cfg(unix)]
#[test]
fn a_live_session_recorded_through_a_symlinked_root_keeps_the_checkout() {
    // The host-state root reached through a symlink (`/tmp` against
    // `/private/tmp`) records a live session's worktree under a different
    // spelling than the closed one. It is still the same checkout.
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let alias_root = fx.repo.parent().unwrap().join("host-state-alias");
    std::os::unix::fs::symlink(worktree.parent().unwrap(), &alias_root).unwrap();
    let alias = alias_root.join(worktree.file_name().unwrap());
    let live = broker
        .store()
        .register_session(&aethyme_broker::NewSession {
            worktree_path: alias.to_string_lossy().into_owned(),
            branch: "live-through-alias".into(),
            origin: aethyme_broker::SessionOrigin::Adopted,
            task: Some("working through the alias".into()),
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
            agent_identity: None,
        })
        .unwrap();
    let report = run(&mut broker);
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(
        kept_reason(&report, &worktree).contains(&format!("session {} is still open", live.id)),
        "{report:#?}"
    );
    assert!(worktree.exists());
}

#[test]
fn an_unclaimed_worktree_is_never_removed() {
    // A worktree no session record names -- a stray `git worktree add`, or a
    // live session's own checkout the broker failed to match -- has no owner
    // the proofs can be about, so it is never a candidate.
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, owned, _) = merged_closed_session(&fx, &mut broker, true);
    let stray = owned.parent().unwrap().join("unclaimed-stray");
    git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            stray.to_str().unwrap(),
            "origin/main",
        ],
    );
    let report = run(&mut broker);
    assert!(stray.exists(), "{report:#?}");
    let stray_text = std::fs::canonicalize(&stray)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        !report
            .removed
            .iter()
            .any(|removed| removed.worktree == stray_text),
        "{report:#?}"
    );
}

#[cfg(unix)]
#[test]
fn automatic_cleanup_honors_default_close_grace() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (id, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::remove_file(fx.repo.join(".aethyme/broker.toml")).unwrap();

    let report = run(&mut broker);
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(kept_reason(&report, &worktree).contains("360 hour closed-worktree grace"));
    assert!(worktree.is_dir());
    // The same checkout is still eligible for the operator's explicit cleanup.
    assert!(
        broker
            .cleanup_plan()
            .unwrap()
            .worktrees
            .iter()
            .any(|item| { item.session_id == id && item.eligible() })
    );
}

#[cfg(unix)]
#[test]
fn automatic_cleanup_rechecks_grace_policy_before_apply() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let plan = broker.auto_cleanup_plan_now(None).unwrap().unwrap();
    assert_eq!(plan.selected_worktrees().len(), 1);
    std::fs::write(
        fx.repo.join(".aethyme/broker.toml"),
        "[retention]\nclosed_worktree_grace_hours = 24\n",
    )
    .unwrap();

    let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(kept_reason(&report, &worktree).contains("closed-worktree grace"));
    assert!(worktree.is_dir());

    std::fs::write(
        fx.repo.join(".aethyme/broker.toml"),
        "[retention]\nclosed_worktree_grace_hours = 0\n",
    )
    .unwrap();
    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    assert!(!worktree.exists());
}

#[cfg(unix)]
#[test]
fn automatic_cleanup_preserves_checkout_when_retention_becomes_unreadable() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    let plan = broker.auto_cleanup_plan_now(None).unwrap().unwrap();
    assert_eq!(plan.selected_worktrees().len(), 1);
    std::fs::write(fx.repo.join(".aethyme/broker.toml"), "[retention\n").unwrap();

    let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(kept_reason(&report, &worktree).contains("retention policy unreadable"));
    assert!(worktree.is_dir());
}

#[cfg(unix)]
#[test]
fn automatic_cleanup_releases_checkout_after_grace_expires() {
    let fx = fixture();
    let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
    let (id, worktree, _) = merged_closed_session(&fx, &mut broker, true);
    std::fs::write(
        fx.repo.join(".aethyme/broker.toml"),
        "[retention]\nclosed_worktree_grace_hours = 24\n",
    )
    .unwrap();
    let report = run(&mut broker);
    assert!(report.removed.is_empty(), "{report:#?}");
    assert!(kept_reason(&report, &worktree).contains("closed-worktree grace"));

    // Move only this fixture's close time beyond the grace window. No sleep
    // or weakened delivery/ownership proof is needed to make cleanup progress.
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let writer = rusqlite::Connection::open(broker.store().db_path()).unwrap();
    writer
        .execute(
            "UPDATE sessions SET closed_at = ?1 WHERE id = ?2",
            rusqlite::params![now_ms - 25 * 3_600_000, id],
        )
        .unwrap();
    drop(writer);
    let report = run(&mut broker);
    assert_eq!(report.removed.len(), 1, "{report:#?}");
    assert!(!worktree.exists());
}

#[cfg(unix)]
#[test]
fn apply_preserves_selected_checkout_after_cleanup_policy_changes() {
    for change in [
        "disabled",
        "pinned",
        "withdrawn_regenerable",
        "strict",
        "unreadable",
    ] {
        let fx = fixture();
        let mut broker = common::open_fixture_broker(&fx.repo, fx._tmp.path()).unwrap();
        let (_, worktree, _) = merged_closed_session(&fx, &mut broker, true);
        let config = fx.repo.join(".aethyme/config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let initial = "[cleanup]\nregenerable = [\".gradle-cache\"]\n[retention]\nclosed_worktree_grace_hours = 0\n";
        std::fs::write(&config, initial).unwrap();
        let plan = broker.auto_cleanup_plan_now(None).unwrap().unwrap();
        assert_eq!(
            plan.selected_worktrees(),
            vec![worktree.to_string_lossy().into_owned()],
            "{change}: must actually select the checkout"
        );
        let replacement = match change {
            "disabled" => "[cleanup]\nauto_remove = false\n".to_owned(),
            "pinned" => format!(
                "[cleanup]\nkeep = [\"{}\"]\n",
                worktree.file_name().unwrap().to_string_lossy()
            ),
            "withdrawn_regenerable" => "[cleanup]\nregenerable = []\n".to_owned(),
            "strict" => "[cleanup]\nstrict = true\n".to_owned(),
            "unreadable" => "[cleanup\n".to_owned(),
            _ => unreachable!(),
        };
        std::fs::write(&config, replacement).unwrap();
        let report = broker.auto_cleanup_apply_now(plan).unwrap().unwrap();
        assert!(report.removed.is_empty(), "{change}: {report:#?}");
        assert!(
            worktree.exists(),
            "{change}: changed policy must retain source"
        );
        let reason = kept_reason(&report, &worktree);
        assert!(
            reason.contains("cleanup policy") && reason.contains("before removal"),
            "{change}: {reason}"
        );
        if change == "disabled" || change == "unreadable" {
            assert!(!report.enabled, "{change}: {report:#?}");
        }
        assert_eq!(report.config_error.is_some(), change == "unreadable");
        // A changed plan never becomes authority later. Only a fresh plan
        // under the restored policy may remove the eligible checkout.
        std::fs::write(&config, initial).unwrap();
        assert_eq!(run(&mut broker).removed.len(), 1, "{change}");
        assert!(!worktree.exists(), "{change}");
    }
}
