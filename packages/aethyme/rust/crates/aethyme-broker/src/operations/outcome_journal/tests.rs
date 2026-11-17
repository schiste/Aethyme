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

/// A row in `running` with a run nonce, as this binary starts every run.
fn running_operation(
    broker: &mut Broker,
    session_id: i64,
    provider: OperationProvider,
    repository: &str,
    command: &[&str],
) -> (CoordinatedOperation, String) {
    let created = prepared_operation(broker, session_id, provider, repository, command);
    broker
        .store()
        .start_coordinated_operation_run(created.id, "{}")
        .unwrap()
}

fn prepared_operation(
    broker: &mut Broker,
    session_id: i64,
    provider: OperationProvider,
    repository: &str,
    command: &[&str],
) -> CoordinatedOperation {
    broker
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
        .unwrap()
}

fn journal_for(
    operation: &CoordinatedOperation,
    run_nonce: &str,
    status: OperationStatus,
) -> OutcomeJournal {
    OutcomeJournal {
        schema: OUTCOME_JOURNAL_SCHEMA.into(),
        operation_id: operation.id,
        session_id: operation.session_id,
        command_sha256: command_sha256(&operation.command_json),
        operation_created_at: operation.created_at,
        run_nonce: run_nonce.into(),
        writer_pid: operation.pid,
        status: status.as_str().into(),
        exit_code: Some(0),
        details_json: json!({ "result": { "url": "https://github.com/o/r/issues/7" } }).to_string(),
        child_finished_at: now_ms(),
    }
}

fn row(broker: &mut Broker, id: i64) -> CoordinatedOperation {
    broker.store().coordinated_operation(id).unwrap().unwrap()
}

fn set_aside_record(db: &Path, id: i64, kind: &str) -> SetAsideJournal {
    let path = set_aside_path(&journal_path(db, id), kind);
    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
}

fn advice_ids(broker: &mut Broker) -> Vec<(&'static str, String)> {
    outcome_journal_advice(broker.store_ref(), now_ms())
        .into_iter()
        .map(|advice| (advice.id, advice.summary))
        .collect()
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
    let (operation, nonce) = running_operation(
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
    let released = std::cell::Cell::new(false);
    let recorded = {
        let _budget = ShortBudget::set(Duration::from_millis(200));
        fixture
            .broker
            .record_coordinated_outcome(
                &operation,
                &nonce,
                OperationStatus::Succeeded,
                Some(0),
                &details_json,
                || {
                    released.set(true);
                    Ok(())
                },
            )
            .unwrap()
    };
    assert!(released.get(), "the locks are released once journaled");
    assert_eq!(recorded.operation.status, OperationStatus::Succeeded);
    assert!(recorded.pending.is_some());
    assert!(recorded.conflict.is_none());
    let journal: OutcomeJournal =
        serde_json::from_slice(&std::fs::read(journal_path(&db, operation.id)).unwrap()).unwrap();
    assert_eq!(journal.operation_id, operation.id);
    assert_eq!(journal.session_id, session);
    assert_eq!(journal.run_nonce, nonce);
    assert_eq!(journal.operation_created_at, operation.created_at);
    assert_eq!(journal.writer_pid, i64::from(std::process::id()));
    assert_eq!(
        journal.command_sha256,
        command_sha256(&operation.command_json)
    );
    assert!(journal.details_json.contains("issues/7"));

    // Still busy: replay leaves the journal, and the write-block check treats
    // the row as decided rather than unknown.
    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary.pending, 1);
    assert!(outcome_journal_pending(&db, operation.id));
    let running = row(&mut fixture.broker, operation.id);
    assert!(fixture.broker.has_decided_outcome_journal(&running));
    drop(lock);

    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary.replayed, 1, "{summary:?}");
    let settled = row(&mut fixture.broker, operation.id);
    assert_eq!(settled.status, OperationStatus::Succeeded);
    assert_eq!(
        details(&settled)["result"]["url"],
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
    let again = row(&mut fixture.broker, operation.id);
    assert_eq!(again.updated_at, settled.updated_at);
    assert_eq!(again.details_json, settled.details_json);

    // The same journal reappearing for the settled row is discarded.
    write_journal(
        &journal_path(&db, operation.id),
        &journal_for(&operation, &nonce, OperationStatus::Succeeded),
    )
    .unwrap();
    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary.discarded, 1);
    assert!(!outcome_journal_pending(&db, operation.id));
}

#[test]
fn a_stale_journal_from_another_run_never_settles_a_running_operation() {
    let mut fixture = Fixture::new("outcome-journal-stale");
    let db = fixture.db();
    let session = fixture.session.id;
    let command = ["git", "branch", "x"];
    let (operation, nonce) = running_operation(
        &mut fixture.broker,
        session,
        OperationProvider::Git,
        "local:stale",
        &command,
    );
    let path = journal_path(&db, operation.id);

    // Same id, session and command -- what a database reset reproduces --
    // but written by another run: another nonce.
    let mut other_run = journal_for(&operation, &nonce, OperationStatus::Succeeded);
    other_run.run_nonce = "0".repeat(32);
    write_journal(&path, &other_run).unwrap();
    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary.refused, 1, "{summary:?}");
    assert!(
        set_aside_record(&db, operation.id, "refused")
            .reason
            .contains("run nonce")
    );

    // Another creation time: the id was reused.
    let mut reused_id = journal_for(&operation, &nonce, OperationStatus::Succeeded);
    reused_id.operation_created_at -= 1;
    write_journal(&path, &reused_id).unwrap();
    assert_eq!(
        fixture
            .broker
            .replay_operation_outcome_journals("test")
            .unwrap()
            .refused,
        1
    );
    assert!(
        set_aside_record(&db, operation.id, "refused")
            .reason
            .contains("creation time")
    );
    assert_eq!(
        row(&mut fixture.broker, operation.id).status,
        OperationStatus::Running
    );

    // The row's own run still records normally afterwards.
    let recorded = fixture
        .broker
        .record_coordinated_outcome(
            &operation,
            &nonce,
            OperationStatus::Failed,
            Some(1),
            "{}",
            || Ok(()),
        )
        .unwrap();
    assert!(recorded.conflict.is_none());
    assert_eq!(
        row(&mut fixture.broker, operation.id).status,
        OperationStatus::Failed
    );
}

#[test]
fn a_row_run_by_an_older_binary_is_never_settled_from_a_journal() {
    let mut fixture = Fixture::new("outcome-journal-legacy");
    let db = fixture.db();
    let session = fixture.session.id;
    let prepared = prepared_operation(
        &mut fixture.broker,
        session,
        OperationProvider::Git,
        "local:legacy",
        &["git", "branch", "x"],
    );
    // An older binary moves the row to running without a nonce.
    let legacy = fixture
        .broker
        .store()
        .transition_coordinated_operation(prepared.id, OperationStatus::Running, None, None)
        .unwrap();
    assert_eq!(
        fixture
            .broker
            .store()
            .coordinated_operation_run_nonce(legacy.id)
            .unwrap(),
        None
    );
    write_journal(
        &journal_path(&db, legacy.id),
        &journal_for(&legacy, &"a".repeat(32), OperationStatus::Succeeded),
    )
    .unwrap();
    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary.refused, 1);
    assert!(
        set_aside_record(&db, legacy.id, "refused")
            .reason
            .contains("older binary")
    );
    assert_eq!(
        row(&mut fixture.broker, legacy.id).status,
        OperationStatus::Running
    );
}

#[test]
fn a_journal_settles_only_rows_run_by_its_writer_process() {
    let mut fixture = Fixture::new("outcome-journal-pid");
    let db = fixture.db();
    let session = fixture.session.id;
    let (running, nonce) = running_operation(
        &mut fixture.broker,
        session,
        OperationProvider::Git,
        "local:pid",
        &["git", "branch", "x"],
    );
    let mut foreign = journal_for(&running, &nonce, OperationStatus::Succeeded);
    foreign.writer_pid = running.pid + 1;
    write_journal(&journal_path(&db, running.id), &foreign).unwrap();
    assert_eq!(
        fixture
            .broker
            .replay_operation_outcome_journals("test")
            .unwrap()
            .refused,
        1
    );
    assert!(
        set_aside_record(&db, running.id, "refused")
            .reason
            .contains("process")
    );
    assert_eq!(
        row(&mut fixture.broker, running.id).status,
        OperationStatus::Running
    );

    // The same binding applies to a row already marked unknown.
    let unknown = fixture
        .broker
        .store()
        .transition_coordinated_operation(
            running.id,
            OperationStatus::OutcomeUnknown,
            None,
            Some(r#"{"reason":"process_ended_without_outcome"}"#),
        )
        .unwrap();
    write_journal(&journal_path(&db, unknown.id), &foreign).unwrap();
    assert_eq!(
        fixture
            .broker
            .replay_operation_outcome_journals("test")
            .unwrap()
            .refused,
        1
    );
    assert_eq!(
        row(&mut fixture.broker, unknown.id).status,
        OperationStatus::OutcomeUnknown
    );

    // Its own writer's journal settles the unknown row.
    write_journal(
        &journal_path(&db, unknown.id),
        &journal_for(&unknown, &nonce, OperationStatus::Succeeded),
    )
    .unwrap();
    assert_eq!(
        fixture
            .broker
            .replay_operation_outcome_journals("test")
            .unwrap()
            .replayed,
        1
    );
    let settled = row(&mut fixture.broker, unknown.id);
    assert_eq!(settled.status, OperationStatus::Succeeded);
    assert_eq!(
        details(&settled)["outcome_journal"]["status_before_replay"],
        "outcome_unknown"
    );
}

#[test]
fn a_different_recorded_outcome_is_reported_as_a_conflict_not_accepted() {
    let mut fixture = Fixture::new("outcome-journal-conflict");
    let db = fixture.db();
    let session = fixture.session.id;
    let (operation, nonce) = running_operation(
        &mut fixture.broker,
        session,
        OperationProvider::Github,
        "github.com/o/r",
        &["gh", "pr", "merge", "5", "--squash"],
    );
    // Something else settled the row as failed while the child ran.
    fixture
        .broker
        .store()
        .transition_coordinated_operation(operation.id, OperationStatus::Failed, Some(1), None)
        .unwrap();

    let recorded = fixture
        .broker
        .record_coordinated_outcome(
            &operation,
            &nonce,
            OperationStatus::Succeeded,
            Some(0),
            "{}",
            || Ok(()),
        )
        .unwrap();
    // The observed result is what the command reports.
    assert_eq!(recorded.operation.status, OperationStatus::Succeeded);
    let conflict = recorded.conflict.expect("a conflict is reported");
    assert_eq!(conflict.observed_status, "succeeded");
    assert_eq!(conflict.recorded_status, "failed");
    assert!(
        conflict
            .message()
            .contains(&format!("operation {}", operation.id))
    );
    // The database row is untouched, the journal kept as a conflict.
    assert_eq!(
        row(&mut fixture.broker, operation.id).status,
        OperationStatus::Failed
    );
    assert!(!outcome_journal_pending(&db, operation.id));
    let kept = set_aside_record(&db, operation.id, "conflict");
    assert_eq!(kept.observed_status.as_deref(), Some("succeeded"));
    assert_eq!(kept.recorded_status.as_deref(), Some("failed"));
    assert!(kept.journal.contains(&nonce));
    let events = fixture.broker.store().events_after(0, i64::MAX).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "operation.outcome_conflict")
    );
    let advice = advice_ids(&mut fixture.broker);
    assert!(
        advice
            .iter()
            .any(|(id, summary)| *id == "operation.outcome-journal-conflict"
                && summary.contains(&format!("operation {}", operation.id))),
        "{advice:?}"
    );

    // A matching outcome is not a conflict: a reconciled success agrees with
    // an observed success, and the journal is simply removed.
    let (second, second_nonce) = running_operation(
        &mut fixture.broker,
        session,
        OperationProvider::Github,
        "github.com/o/r",
        &["gh", "pr", "merge", "6", "--squash"],
    );
    fixture
        .broker
        .store()
        .transition_coordinated_operation(
            second.id,
            OperationStatus::ReconciledSucceeded,
            None,
            None,
        )
        .unwrap();
    let recorded = fixture
        .broker
        .record_coordinated_outcome(
            &second,
            &second_nonce,
            OperationStatus::Succeeded,
            Some(0),
            "{}",
            || Ok(()),
        )
        .unwrap();
    assert!(recorded.conflict.is_none());
    assert!(!outcome_journal_pending(&db, second.id));
    assert!(!set_aside_path(&journal_path(&db, second.id), "conflict").exists());
}

#[test]
fn a_replayed_journal_disagreeing_with_a_reconciled_row_is_kept_as_a_conflict() {
    let mut fixture = Fixture::new("outcome-journal-replay-conflict");
    let db = fixture.db();
    let session = fixture.session.id;
    let (operation, nonce) = running_operation(
        &mut fixture.broker,
        session,
        OperationProvider::Git,
        "local:conflict",
        &["git", "branch", "x"],
    );
    fixture
        .broker
        .store()
        .transition_coordinated_operation(
            operation.id,
            OperationStatus::ReconciledFailed,
            None,
            None,
        )
        .unwrap();
    write_journal(
        &journal_path(&db, operation.id),
        &journal_for(&operation, &nonce, OperationStatus::Succeeded),
    )
    .unwrap();
    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary.conflicts, 1, "{summary:?}");
    assert_eq!(
        row(&mut fixture.broker, operation.id).status,
        OperationStatus::ReconciledFailed
    );
    assert!(set_aside_path(&journal_path(&db, operation.id), "conflict").exists());
}

#[test]
fn a_refused_journal_explains_the_write_block_in_status_until_it_is_resolved() {
    let mut fixture = Fixture::new("outcome-journal-mismatch");
    let db = fixture.db();
    let session = fixture.session.id;
    let (operation, nonce) = running_operation(
        &mut fixture.broker,
        session,
        OperationProvider::Git,
        "local:mismatch",
        &["git", "branch", "x"],
    );
    let path = journal_path(&db, operation.id);

    let mut wrong_session = journal_for(&operation, &nonce, OperationStatus::Succeeded);
    wrong_session.session_id = session + 1000;
    write_journal(&path, &wrong_session).unwrap();
    let summary = fixture
        .broker
        .replay_operation_outcome_journals("test")
        .unwrap();
    assert_eq!(summary.refused, 1, "{summary:?}");
    assert_eq!(summary.replayed, 0);
    assert!(!path.exists());
    assert!(
        set_aside_record(&db, operation.id, "refused")
            .reason
            .contains("session")
    );

    let advice = advice_ids(&mut fixture.broker);
    let refused: Vec<_> = advice
        .iter()
        .filter(|(id, _)| *id == "operation.outcome-journal-refused")
        .collect();
    assert_eq!(refused.len(), 1, "{advice:?}");
    assert!(
        refused[0]
            .1
            .contains(&format!("operation {}", operation.id))
    );
    assert!(
        refused[0].1.contains("session"),
        "the reason is named: {advice:?}"
    );

    let mut wrong_command = journal_for(&operation, &nonce, OperationStatus::Succeeded);
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
    assert_eq!(
        row(&mut fixture.broker, operation.id).status,
        OperationStatus::Running
    );

    // Once the operation is reconciled the refusal explains nothing.
    fixture
        .broker
        .store()
        .transition_coordinated_operation(
            operation.id,
            OperationStatus::ReconciledFailed,
            None,
            None,
        )
        .unwrap();
    assert!(
        !advice_ids(&mut fixture.broker)
            .iter()
            .any(|(id, _)| *id == "operation.outcome-journal-refused")
    );
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
    let (ghost, _) = running_operation(
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
        row(&mut reopened, ghost.id).status,
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
        row(&mut reopened, ghost.id).status,
        OperationStatus::OutcomeUnknown
    );
}

/// The interleaving the early lock release exists for: once the journal is
/// on disk, another session can take the repository write lock while the
/// first is still retrying its database record, and the first still records
/// its own outcome afterwards.
#[test]
fn the_coordination_lock_is_released_while_the_record_is_retried() {
    use std::sync::mpsc;

    let mut fixture = Fixture::new("outcome-journal-release");
    let db = fixture.db();
    let worktree = PathBuf::from(&fixture.session.worktree_path);
    let first = fixture
        .broker
        .run_coordinated_operation_at(
            git_write(fixture.session.id, &["branch", "warm"]),
            &worktree,
        )
        .unwrap();
    assert_eq!(
        first.operation.scope, "repository",
        "a repository-wide write"
    );
    let repository = first.operation.repository.clone();
    let main_root = fixture.broker.main_root().to_path_buf();
    let journals = outcome_journal_dir(&db).unwrap();

    let (lock_now_tx, lock_now_rx) = mpsc::channel::<()>();
    let (locked_tx, locked_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder_db = db.clone();
    let holder = std::thread::spawn(move || {
        lock_now_rx.recv().unwrap();
        let connection = hold_write_lock(&holder_db);
        locked_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(20));
        drop(connection);
    });
    let observer_db = db.clone();
    let observer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut observed = None;
        while Instant::now() < deadline {
            let journal = std::fs::read_dir(&journals).ok().and_then(|entries| {
                entries
                    .filter_map(Result::ok)
                    .find_map(|entry| journal_operation_id(entry.file_name().to_str()?))
            });
            if let Some(id) = journal
                && let Some(lock) = crate::operations::RepositoryWriteLock::try_acquire_nonblocking(
                    &main_root,
                    &repository,
                )
                .unwrap()
            {
                // Read while the record is still being retried.
                let status: String = rusqlite::Connection::open(&observer_db)
                    .unwrap()
                    .query_row(
                        "SELECT status FROM coordinated_operations WHERE id = ?1",
                        [id],
                        |row| row.get(0),
                    )
                    .unwrap();
                drop(lock);
                observed = Some((id, status));
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        release_tx.send(()).unwrap();
        observed
    });

    let report = {
        let _budget = ShortBudget::set(Duration::from_secs(30));
        fixture
            .broker
            .run_coordinated_operation_at_with_hooks(
                git_write(fixture.session.id, &["branch", "interleaved"]),
                &worktree,
                QueueWait::Forever,
                || Ok(()),
                |_, _| {
                    lock_now_tx.send(()).unwrap();
                    locked_rx.recv().unwrap();
                    Ok(None)
                },
            )
            .unwrap()
    };
    let observed = observer.join().unwrap();
    holder.join().unwrap();

    let (id, status_while_locked) =
        observed.expect("another writer took the repository lock during the retries");
    assert_eq!(id, report.operation.id);
    assert_eq!(
        status_while_locked, "running",
        "taken before the record landed"
    );
    assert!(report.ok());
    assert!(
        report.outcome_record_pending.is_none(),
        "recorded within the budget"
    );
    assert_eq!(
        row(&mut fixture.broker, id).status,
        OperationStatus::Succeeded
    );
    assert!(!outcome_journal_pending(&db, id));
}

#[test]
fn status_advises_only_about_a_journal_older_than_ten_minutes() {
    let mut fixture = Fixture::new("outcome-journal-advice");
    let db = fixture.db();
    let session = fixture.session.id;
    let (operation, nonce) = running_operation(
        &mut fixture.broker,
        session,
        OperationProvider::Github,
        "github.com/o/r",
        &["gh", "issue", "create", "--title", "t"],
    );
    let now = now_ms();
    let mut journal = journal_for(&operation, &nonce, OperationStatus::Succeeded);
    journal.child_finished_at = now - 60_000;
    write_journal(&journal_path(&db, operation.id), &journal).unwrap();
    assert!(outcome_journal_advice(fixture.broker.store_ref(), now).is_empty());

    journal.child_finished_at = now - UNREPLAYED_ADVICE_AFTER_MS - 1_000;
    write_journal(&journal_path(&db, operation.id), &journal).unwrap();
    let advice = outcome_journal_advice(fixture.broker.store_ref(), now);
    assert_eq!(advice.len(), 1);
    assert_eq!(advice[0].id, "operation.outcome-journal-unreplayed");
    assert_eq!(
        advice[0].commands,
        vec![format!(
            "aethyme broker advanced operations show {} --json",
            operation.id
        )]
    );
}

#[test]
fn journal_file_names_round_trip_and_reject_strangers() {
    assert_eq!(journal_operation_id(&journal_file_name(17)), Some(17));
    assert_eq!(journal_operation_id("operation-17.json.refused"), None);
    assert_eq!(journal_operation_id(".operation-17.json-abc"), None);
    assert_eq!(journal_operation_id("operation-0.json"), None);
    assert_eq!(journal_operation_id("operation--3.json"), None);
    assert_eq!(
        set_aside_operation_id("operation-17.json.refused"),
        Some((17, "refused"))
    );
    assert_eq!(
        set_aside_operation_id("operation-17.json.conflict"),
        Some((17, "conflict"))
    );
    assert_eq!(set_aside_operation_id("operation-17.json"), None);
}
