//! Work a merged pull request landed is delivered, whatever its merge style.
//!
//! In a repository that does not promote, the fetched default branch is the
//! only place work lands. A squash rewrites the session's commits, so only
//! their content reaches it; `finish` used to accept that content only behind
//! a promoted queue row or a hand-recorded representation, neither of which
//! such a repository ever produces, and blocked with advice that could not
//! resolve the state.

use std::path::Path;
use std::process::Command;

mod common;

use aethyme_broker::{FinishOptions, FinishStatus, GcRowKind, MergeStatus, events};
use aethyme_testkit::MergeStyle;
use common::{FixtureRepository, open_fixture_repository_broker};

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

/// A verify-only repository (the default) whose default branch is on a local
/// `origin` and authorizes pushing session branches.
fn fixture() -> (FixtureRepository, common::FixtureBroker) {
    let fixture = FixtureRepository::new();
    git(fixture.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(fixture.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(fixture.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::create_dir_all(fixture.path().join(".aethyme")).unwrap();
    std::fs::write(
        fixture.path().join(".aethyme/config.toml"),
        "[delivery]\npush_session_branches = true\n",
    )
    .unwrap();
    git(fixture.path(), &["add", "-A"]);
    // `/.aethyme/` is ignored; the policy file is tracked on purpose.
    git(fixture.path(), &["add", "-f", ".aethyme/config.toml"]);
    git(fixture.path(), &["commit", "-qm", "init"]);
    aethyme_testkit::ensure_origin(fixture.path());
    let broker = open_fixture_repository_broker(&fixture).unwrap();
    (fixture, broker)
}

fn commit(worktree: &Path, file: &str, message: &str) {
    std::fs::write(worktree.join(file), format!("{message}\n")).unwrap();
    git(worktree, &["add", "-A"]);
    git(worktree, &["commit", "-qm", message]);
}

/// A session with one verified commit, landed by a pull request merged with
/// `style`. Returns the session id and its verified queue entry.
fn landed_session(broker: &mut common::FixtureBroker, style: MergeStyle) -> (i64, i64) {
    let session = broker.start_worktree("land by pull request", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    commit(&worktree, "landed.txt", "landed work");
    let submitted = broker.submit(session.id).unwrap();
    assert_eq!(submitted.entry.status, MergeStatus::Verified);
    aethyme_testkit::land_as_pull_request(broker.main_root(), &session.branch, style);
    if style == MergeStyle::Squash {
        // "Delete branch on merge": the session head is now on no remote ref,
        // so only the default branch's content proves the work landed.
        aethyme_testkit::delete_remote_branch(broker.main_root(), &session.branch);
    }
    (session.id, submitted.entry.id)
}

fn keep() -> FinishOptions {
    FinishOptions {
        keep_worktree: true,
    }
}

#[test]
fn a_squash_merged_pull_request_finishes_the_session() {
    let (_fixture, mut broker) = fixture();
    let (session, entry) = landed_session(&mut broker, MergeStyle::Squash);

    let report = broker.finish_with_options(session, keep()).unwrap();

    assert_eq!(report.status, FinishStatus::Closed, "{report:#?}");
    assert!(report.closed);
    assert_eq!(report.unsubmitted_commits, 0, "{report:#?}");
    assert!(report.delivery.published, "{report:#?}");
    assert!(report.representation.is_none(), "no record was needed");
    assert!(
        !report
            .next_commands
            .iter()
            .any(|command| command.contains("representation")),
        "{report:#?}"
    );
    assert_eq!(
        broker.store().merge_queue_entry(entry).unwrap().status,
        MergeStatus::ExternallyLanded
    );
}

/// A merge commit already finished by ancestry; it must keep doing so, and
/// its verified entry becomes terminal too.
#[test]
fn a_merge_commit_pull_request_finishes_the_session() {
    let (_fixture, mut broker) = fixture();
    let (session, entry) = landed_session(&mut broker, MergeStyle::MergeCommit);

    let report = broker.finish_with_options(session, keep()).unwrap();

    assert_eq!(report.status, FinishStatus::Closed, "{report:#?}");
    assert!(report.delivery.published, "{report:#?}");
    assert_eq!(
        report.latest_queue_status,
        Some(MergeStatus::ExternallyLanded)
    );
    assert_eq!(
        broker.store().merge_queue_entry(entry).unwrap().status,
        MergeStatus::ExternallyLanded
    );
}

/// Work only partly on the default branch is not landed: finish still
/// refuses, names steps that can change that, and honours `--abandon`.
#[test]
fn work_partly_on_the_default_branch_blocks_with_a_resolvable_next_step() {
    let (_fixture, mut broker) = fixture();
    let (session, entry) = landed_session(&mut broker, MergeStyle::Squash);
    let worktree = std::path::PathBuf::from(broker.store().session(session).unwrap().worktree_path);
    commit(&worktree, "later.txt", "work after the merge");

    let blocked = broker.finish_with_options(session, keep()).unwrap();

    assert_eq!(blocked.status, FinishStatus::Blocked, "{blocked:#?}");
    assert!(!blocked.closed);
    assert!(blocked.unsubmitted_commits > 0, "{blocked:#?}");
    assert!(!blocked.delivery.published, "{blocked:#?}");
    assert_eq!(
        blocked.next_commands,
        vec![
            format!("aethyme broker push --session {session} --pr"),
            format!("aethyme broker finish --session {session} --abandon --reason \"<why>\""),
        ]
    );
    assert_eq!(
        broker.store().merge_queue_entry(entry).unwrap().status,
        MergeStatus::Verified,
        "nothing landed the current head, so its entry is not terminal"
    );

    let abandoned = broker
        .finish_abandoning_unpushed(session, keep(), "superseded by another change")
        .unwrap();
    assert_eq!(abandoned.status, FinishStatus::Closed, "{abandoned:#?}");
    assert!(abandoned.closed);
    let recorded = broker
        .store()
        .events_after(0, i64::MAX)
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.kind == events::BROKER_SESSION_ABANDONED_UNPUSHED
                && event.session_id == Some(session)
        })
        .count();
    assert_eq!(recorded, 1, "the abandonment is recorded exactly once");
}

/// A verified entry used to stay verified forever, outside every retention
/// rule. Once finish makes it terminal, retention prunes it.
#[test]
fn a_landed_verified_entry_is_pruned_by_retention() {
    let (fixture, mut broker) = fixture();
    let (session, entry) = landed_session(&mut broker, MergeStyle::Squash);
    assert!(broker.finish_with_options(session, keep()).unwrap().closed);
    drop(broker);

    let database =
        rusqlite::Connection::open(fixture.path().join(aethyme_broker::BROKER_DB_RELPATH)).unwrap();
    database
        .execute(
            "UPDATE merge_queue SET created_at = 1, updated_at = 1 WHERE id = ?1",
            [entry],
        )
        .unwrap();
    drop(database);

    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    let plan = broker.gc_plan().unwrap();
    assert!(
        plan.rows
            .iter()
            .any(|row| row.kind == GcRowKind::MergeQueue && row.id == entry),
        "{:#?}",
        plan.rows
    );
}
