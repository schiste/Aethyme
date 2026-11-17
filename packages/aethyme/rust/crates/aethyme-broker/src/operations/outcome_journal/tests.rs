use std::cell::RefCell;
use std::path::Path;

use super::*;
use crate::operations::{CoordinatedCommand, QueueWait};
use crate::test_storage::TestStorage;
use crate::types::{
    NewCoordinatedOperation, OperationEffect, OperationIdentityProvenance, OperationProvider,
};

fn git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
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

struct Fixture {
    repo: tempfile::TempDir,
    storage: TestStorage,
    broker: Broker,
    session: crate::Session,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        git(repo.path(), &["commit", "--allow-empty", "-qm", "base"]);
        let storage = TestStorage::new();
        let mut broker = storage.open(repo.path()).unwrap();
        let session = broker.start_worktree(name, None).unwrap();
        Self {
            repo,
            storage,
            broker,
            session,
        }
    }

    fn db(&mut self) -> PathBuf {
        self.broker.store().db_path().to_path_buf()
    }

    fn reopen(&self) -> Broker {
        self.storage.open(self.repo.path()).unwrap()
    }
}

fn git_write(session_id: i64, args: &[&str]) -> CoordinatedCommand {
    CoordinatedCommand {
        session_id,
        provider: OperationProvider::Git,
        repository: None,
        resolved_target: None,
        scope: None,
        declared_effect: None,
        destructive_confirmed: false,
        cross_session: None,
        ref_write_acknowledged: false,
        authorization_reason: Some("outcome journal test".into()),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
    }
}

/// Another process's write transaction: holds the database write lock until
/// the returned connection is dropped.
fn hold_write_lock(db: &Path) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open(db).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    connection
}

/// Restores the default retry budget even when an assertion fails.
struct ShortBudget;

impl ShortBudget {
    fn set(budget: Duration) -> Self {
        set_record_retry_budget_for_test(Some(budget));
        Self
    }
}

impl Drop for ShortBudget {
    fn drop(&mut self) {
        set_record_retry_budget_for_test(None);
    }
}

/// Run a real coordinated `git` write whose child succeeds, while another
/// connection takes the write lock right after the child exits and keeps it
/// past the retry budget. Returns the report and the still-held lock.
fn run_with_database_locked_after_the_child(
    fixture: &mut Fixture,
    args: &[&str],
) -> (crate::CoordinatedOperationReport, rusqlite::Connection) {
    let db = fixture.db();
    let holder: RefCell<Option<rusqlite::Connection>> = RefCell::new(None);
    let worktree = PathBuf::from(&fixture.session.worktree_path);
    let report = {
        let _budget = ShortBudget::set(Duration::from_millis(300));
        fixture
            .broker
            .run_coordinated_operation_at_with_hooks(
                git_write(fixture.session.id, args),
                &worktree,
                QueueWait::Forever,
                || Ok(()),
                |_, _| {
                    *holder.borrow_mut() = Some(hold_write_lock(&db));
                    Ok(None)
                },
            )
            .unwrap()
    };
    let lock = holder
        .into_inner()
        .expect("the child succeeded and the lock was taken");
    (report, lock)
}

fn details(operation: &CoordinatedOperation) -> serde_json::Value {
    serde_json::from_str(operation.details_json.as_deref().unwrap()).unwrap()
}

fn running_operation(
    broker: &mut Broker,
    session_id: i64,
    provider: OperationProvider,
    repository: &str,
    command: &[&str],
) -> CoordinatedOperation {
    let created = broker
        .store()
        .create_coordinated_operation(&NewCoordinatedOperation {
            session_id,
            provider,
            repository: repository.into(),
            scope: "repository".into(),
            effect: OperationEffect::Write,
            authorization_reason: Some("outcome journal test".into()),
            command_json: serde_json::to_string(command).unwrap(),
            pid: i64::from(std::process::id()),
            host_operation_id: None,
            identity_provenance: OperationIdentityProvenance::LocalRepository,
        })
        .unwrap();
    broker
        .store()
        .transition_coordinated_operation(created.id, OperationStatus::Running, None, None)
        .unwrap()
}

fn journal_for(operation: &CoordinatedOperation, status: OperationStatus) -> OutcomeJournal {
    OutcomeJournal {
        schema: OUTCOME_JOURNAL_SCHEMA.into(),
        operation_id: operation.id,
        session_id: operation.session_id,
        command_sha256: command_sha256(&operation.command_json),
        status: status.as_str().into(),
        exit_code: Some(0),
        details_json: json!({ "result": { "url": "https://github.com/o/r/issues/7" } }).to_string(),
        child_finished_at: now_ms(),
        writer_pid: 1,
    }
}

fn journal_path(db: &Path, operation_id: i64) -> PathBuf {
    outcome_journal_dir(db)
        .unwrap()
        .join(journal_file_name(operation_id))
}

#[test]
fn a_success_recorded_while_the_database_is_busy_is_replayed_by_the_next_open() {
    let mut fixture = Fixture::new("outcome-journal-open");
    let db = fixture.db();
    let (report, lock) = run_with_database_locked_after_the_child(&mut fixture, &["branch", "a"]);
    let id = report.operation.id;

    // The command reports the child's true result, not a database error.
    assert!(report.command_success);
    assert!(report.ok(), "{:?}", report.operation.status);
    assert_eq!(report.operation.status, OperationStatus::Succeeded);
    assert_eq!(report.operation.exit_code, Some(0));
    let pending = report
        .outcome_record_pending
        .clone()
        .expect("the record is pending");
    assert_eq!(pending.operation_id, id);
    assert!(pending.attempts >= 1);
    assert!(pending.waited_ms >= 300, "{pending:?}");
    assert!(pending.database_error.contains("locked"), "{pending:?}");
    let message = pending.message(report.operation.status);
    assert!(message.contains(&format!("operation {id} ")), "{message}");
    assert!(message.contains("recorded automatically"), "{message}");

    // The journal holds it; the row still says running.
    assert!(outcome_journal_pending(&db, id));
    let row = fixture
        .broker
        .store()
        .coordinated_operation(id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, OperationStatus::Running);
    assert_eq!(
        fixture
            .broker
            .show_coordinated_operation(id)
            .unwrap()
            .outcome_journal,
        "pending"
    );

    drop(lock);
    // Any later writable open records it.
    let mut reopened = fixture.reopen();
    let row = reopened.store().coordinated_operation(id).unwrap().unwrap();
    assert_eq!(row.status, OperationStatus::Succeeded);
    assert_eq!(row.exit_code, Some(0));
    assert!(row.finished_at.is_some());
    let replay = &details(&row)["outcome_journal"];
    assert_eq!(replay["replayed_by"], "broker_open");
    assert_eq!(replay["status_before_replay"], "running");
    assert!(!outcome_journal_pending(&db, id));
    assert_eq!(
        reopened
            .show_coordinated_operation(id)
            .unwrap()
            .outcome_journal,
        "none"
    );
    let events = reopened.store().events_after(0, i64::MAX).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "operation.succeeded")
    );

    // A second session writes freely: nothing is write-blocked.
    let second = reopened
        .start_worktree("outcome-journal-second", None)
        .unwrap();
    let next = reopened
        .run_coordinated_operation_at(
            git_write(second.id, &["branch", "b"]),
            Path::new(&second.worktree_path),
        )
        .unwrap();
    assert!(next.ok());
    assert!(next.outcome_record_pending.is_none());
}

#[test]
fn a_coordinated_write_replays_a_pending_journal_before_its_write_block_check() {
    let mut fixture = Fixture::new("outcome-journal-write");
    let db = fixture.db();
    let second = fixture
        .broker
        .start_worktree("outcome-journal-write-second", None)
        .unwrap();
    let (report, lock) = run_with_database_locked_after_the_child(&mut fixture, &["branch", "a"]);
    let id = report.operation.id;
    assert!(report.outcome_record_pending.is_some());
    drop(lock);

    // Same broker, no reopen: the write path itself replays first. Without
    // the journal this row would be turned `outcome_unknown` and block.
    let next = fixture
        .broker
        .run_coordinated_operation_at(
            git_write(second.id, &["branch", "b"]),
            Path::new(&second.worktree_path),
        )
        .unwrap();
    assert!(next.ok());
    let row = fixture
        .broker
        .store()
        .coordinated_operation(id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, OperationStatus::Succeeded);
    assert_eq!(
        details(&row)["outcome_journal"]["replayed_by"],
        "write_block_check"
    );
    assert!(!outcome_journal_pending(&db, id));
}

#[test]
fn a_github_outcome_recorded_while_busy_is_journaled_and_replayed_idempotently() {
    let mut fixture = Fixture::new("outcome-journal-gh");
    let db = fixture.db();
    let session = fixture.session.id;
    let operation = running_operation(
        &mut fixture.broker,
        session,
        OperationProvider::Github,
        "github.com/o/r",
        &[
            "gh",
            "issue",
            "create",
            "--title",
            "t",
            "--body-file",
            "b.md",
        ],
    );
    let details_json =
        json!({ "result": { "url": "https://github.com/o/r/issues/7" } }).to_string();

    let lock = hold_write_lock(&db);
    let recorded = {
        let _budget = ShortBudget::set(Duration::from_millis(200));
        fixture
            .broker
            .record_coordinated_outcome(
                &operation,
                OperationStatus::Succeeded,
                Some(0),
                &details_json,
            )
            .unwrap()
    };
    assert_eq!(recorded.operation.status, OperationStatus::Succeeded);
    assert!(recorded.pending.is_some());
    let journal: OutcomeJournal =
        serde_json::from_slice(&std::fs::read(journal_path(&db, operation.id)).unwrap()).unwrap();
    assert_eq!(journal.operation_id, operation.id);
    assert_eq!(journal.session_id, session);
    assert_eq!(
        journal.command_sha256,
        command_sha256(&operation.command_json)
    );
    assert!(journal.details_json.contains("issues/7"));

    // Still busy: replay leaves the journal for the next opener.
    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary.pending, 1);
    assert!(outcome_journal_pending(&db, operation.id));
    drop(lock);

    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary.replayed, 1, "{summary:?}");
    let row = fixture
        .broker
        .store()
        .coordinated_operation(operation.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, OperationStatus::Succeeded);
    assert_eq!(
        details(&row)["result"]["url"],
        "https://github.com/o/r/issues/7"
    );
    assert!(
        fixture
            .broker
            .store()
            .unresolved_coordinated_operations("github.com/o/r")
            .unwrap()
            .is_empty(),
        "a replayed success leaves nothing to write-block"
    );

    // Idempotent: nothing left to do, the row is untouched.
    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary, OutcomeReplaySummary::default());
    let again = fixture
        .broker
        .store()
        .coordinated_operation(operation.id)
        .unwrap()
        .unwrap();
    assert_eq!(again.updated_at, row.updated_at);
    assert_eq!(again.details_json, row.details_json);

    // A journal reappearing for the settled row is discarded, not applied.
    write_journal(
        &journal_path(&db, operation.id),
        &journal_for(&row, OperationStatus::Failed),
    )
    .unwrap();
    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary.discarded, 1);
    assert!(!outcome_journal_pending(&db, operation.id));
    let after = fixture
        .broker
        .store()
        .coordinated_operation(operation.id)
        .unwrap()
        .unwrap();
    assert_eq!(after.status, OperationStatus::Succeeded);
}

#[test]
fn a_journal_that_does_not_match_its_operation_is_refused_and_never_applied() {
    let mut fixture = Fixture::new("outcome-journal-mismatch");
    let db = fixture.db();
    let session = fixture.session.id;
    let operation = running_operation(
        &mut fixture.broker,
        session,
        OperationProvider::Git,
        "local:mismatch",
        &["git", "branch", "x"],
    );

    let mut wrong_session = journal_for(&operation, OperationStatus::Succeeded);
    wrong_session.session_id = session + 1000;
    let path = journal_path(&db, operation.id);
    write_journal(&path, &wrong_session).unwrap();
    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary.refused, 1, "{summary:?}");
    assert_eq!(summary.replayed, 0);
    assert!(!path.exists());
    let mut refused = path.as_os_str().to_owned();
    refused.push(".refused");
    assert!(PathBuf::from(&refused).exists(), "kept for inspection");
    std::fs::remove_file(&refused).unwrap();

    let mut wrong_command = journal_for(&operation, OperationStatus::Succeeded);
    wrong_command.command_sha256 = command_sha256("[\"git\",\"branch\",\"other\"]");
    write_journal(&path, &wrong_command).unwrap();
    assert_eq!(
        fixture
            .broker
            .replay_operation_outcome_journals("test")
            .unwrap()
            .refused,
        1
    );

    std::fs::write(&path, b"{ not json").unwrap();
    assert_eq!(
        fixture
            .broker
            .replay_operation_outcome_journals("test")
            .unwrap()
            .refused,
        1
    );

    let row = fixture
        .broker
        .store()
        .coordinated_operation(operation.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, OperationStatus::Running, "nothing was applied");
}

#[test]
fn without_a_journal_an_interrupted_write_still_has_an_unknown_outcome() {
    let mut fixture = Fixture::new("outcome-journal-none");
    let worktree = PathBuf::from(&fixture.session.worktree_path);
    let first = fixture
        .broker
        .run_coordinated_operation_at(git_write(fixture.session.id, &["branch", "a"]), &worktree)
        .unwrap();
    assert!(first.ok());
    let session = fixture.session.id;
    // A process that died after starting its child, before the result was
    // known: `running`, and no journal.
    let ghost = running_operation(
        &mut fixture.broker,
        session,
        OperationProvider::Git,
        &first.operation.repository,
        &["git", "branch", "ghost"],
    );

    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary, OutcomeReplaySummary::default());
    let mut reopened = fixture.reopen();
    assert_eq!(
        reopened
            .store()
            .coordinated_operation(ghost.id)
            .unwrap()
            .unwrap()
            .status,
        OperationStatus::Running,
        "nothing infers an outcome without a journal"
    );

    let blocked = reopened
        .run_coordinated_operation_at(git_write(session, &["branch", "b"]), &worktree)
        .unwrap_err();
    match blocked {
        BrokerOpError::CoordinatedOperationBlocked { operation_id, .. } => {
            assert_eq!(operation_id, ghost.id)
        }
        other => panic!("expected a write block, got {other:?}"),
    }
    assert_eq!(
        reopened
            .store()
            .coordinated_operation(ghost.id)
            .unwrap()
            .unwrap()
            .status,
        OperationStatus::OutcomeUnknown
    );
}

#[test]
fn a_journal_settles_an_operation_already_marked_unknown() {
    let mut fixture = Fixture::new("outcome-journal-unknown");
    let db = fixture.db();
    let session = fixture.session.id;
    let operation = running_operation(
        &mut fixture.broker,
        session,
        OperationProvider::Git,
        "local:unknown",
        &["git", "branch", "x"],
    );
    let unknown = fixture
        .broker
        .store()
        .transition_coordinated_operation(
            operation.id,
            OperationStatus::OutcomeUnknown,
            None,
            Some(r#"{"reason":"process_ended_without_outcome"}"#),
        )
        .unwrap();
    write_journal(
        &journal_path(&db, operation.id),
        &journal_for(&unknown, OperationStatus::Succeeded),
    )
    .unwrap();
    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary.replayed, 1);
    let row = fixture
        .broker
        .store()
        .coordinated_operation(operation.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.status, OperationStatus::Succeeded);
    assert_eq!(
        details(&row)["outcome_journal"]["status_before_replay"],
        "outcome_unknown"
    );
}

#[test]
fn status_advises_only_about_a_journal_older_than_ten_minutes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("broker.db");
    let now = now_ms();
    let operation = CoordinatedOperation {
        id: 42,
        session_id: 3,
        provider: OperationProvider::Github,
        repository: "github.com/o/r".into(),
        scope: "repository".into(),
        effect: OperationEffect::Write,
        status: OperationStatus::Running,
        authorization_reason: None,
        command_json: "[]".into(),
        pid: 1,
        exit_code: None,
        details_json: None,
        created_at: now,
        updated_at: now,
        finished_at: None,
        host_operation_id: None,
        identity_provenance: OperationIdentityProvenance::LocalRepository,
        agent_provenance: None,
    };
    let mut journal = journal_for(&operation, OperationStatus::Succeeded);
    journal.child_finished_at = now - 60_000;
    write_journal(&journal_path(&db, 42), &journal).unwrap();
    assert!(unreplayed_outcome_journal_advice(&db, now).is_empty());

    journal.child_finished_at = now - UNREPLAYED_ADVICE_AFTER_MS - 1_000;
    write_journal(&journal_path(&db, 42), &journal).unwrap();
    let advice = unreplayed_outcome_journal_advice(&db, now);
    assert_eq!(advice.len(), 1);
    assert_eq!(advice[0].id, "operation.outcome-journal-unreplayed");
    assert_eq!(
        advice[0].commands,
        vec!["aethyme broker advanced operations show 42 --json".to_string()]
    );
}

#[test]
fn journal_file_names_round_trip_and_reject_strangers() {
    assert_eq!(journal_operation_id(&journal_file_name(17)), Some(17));
    assert_eq!(journal_operation_id("operation-17.json.refused"), None);
    assert_eq!(journal_operation_id(".operation-17.json-abc"), None);
    assert_eq!(journal_operation_id("operation-0.json"), None);
    assert_eq!(journal_operation_id("operation--3.json"), None);
}
