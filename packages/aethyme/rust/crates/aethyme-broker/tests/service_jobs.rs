use std::sync::{Arc, Barrier};

use aethyme_broker::service::jobs::{
    JobCompletion, JobError, JobOperation, JobProgress, JobRequest, JobState, JobStore,
};

fn request(key: &str) -> JobRequest {
    JobRequest {
        repository: "verified-repository-a".into(),
        idempotency_key: key.into(),
        operation: JobOperation::GcPlan {
            include_active_gate_cache: false,
            full_scan: false,
        },
        deadline_ms: 1000,
    }
}

#[test]
fn retries_and_joiners_keep_their_original_job_after_completion() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    let mut store = JobStore::open(&path).unwrap();
    let first = store.submit(&request("first"), 10).unwrap();
    assert!(!first.joined);
    let joined = store.submit(&request("joined"), 11).unwrap();
    assert!(joined.joined);
    assert_eq!(joined.job.id, first.job.id);
    let claim = store.claim_next("worker-a", 12).unwrap().unwrap();
    let completion = JobCompletion::Succeeded {
        result: serde_json::json!({"digest":"review-this-plan"}),
    };
    let done = store.complete(&claim, &completion, 13).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    drop(store);
    let mut reopened = JobStore::open(&path).unwrap();
    // A retry is a lookup even when its original deadline is now in the past.
    for key in ["first", "joined"] {
        let retry = reopened.submit(&request(key), 2000).unwrap();
        assert!(retry.joined);
        assert_eq!(retry.job, done);
        assert_eq!(
            reopened.find_request("verified-repository-a", key).unwrap(),
            Some(done.clone())
        );
    }
    let fresh = reopened.submit(&request("fresh"), 20).unwrap();
    assert!(!fresh.joined);
    assert_ne!(fresh.job.id, done.id);
    assert_eq!(done.completion, Some(completion));
}

#[test]
fn identity_includes_repository_options_and_deadline() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(&tmp.path().join("jobs.db")).unwrap();
    let original = store.submit(&request("key"), 1).unwrap().job;
    let mut other_repo = request("key");
    other_repo.repository = "verified-repository-b".into();
    let mut other_options = request("other-options");
    other_options.operation = JobOperation::GcPlan {
        include_active_gate_cache: true,
        full_scan: false,
    };
    let mut other_deadline = request("other-deadline");
    other_deadline.deadline_ms += 1;
    let ids = [
        original.id,
        store.submit(&other_repo, 2).unwrap().job.id,
        store.submit(&other_options, 2).unwrap().job.id,
        store.submit(&other_deadline, 2).unwrap().job.id,
    ];
    assert_eq!(
        ids.into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        4
    );
    other_options.idempotency_key = "key".into();
    assert!(matches!(
        store.submit(&other_options, 3),
        Err(JobError::IdempotencyConflict)
    ));
    other_deadline.idempotency_key = "key".into();
    assert!(matches!(
        store.submit(&other_deadline, 3),
        Err(JobError::IdempotencyConflict)
    ));
    assert_eq!(store.get(original.id).unwrap(), original);
}

#[test]
fn independent_connections_submit_and_claim_once() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    JobStore::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|n| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut store = JobStore::open(&path).unwrap();
                barrier.wait();
                let submission = store.submit(&request(&format!("request-{n}")), 10).unwrap();
                let claim = store.claim_next(&format!("worker-{n}"), 11).unwrap();
                (submission, claim)
            })
        })
        .collect();
    let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(outcomes.iter().filter(|(s, _)| !s.joined).count(), 1);
    assert_eq!(outcomes.iter().filter(|(_, c)| c.is_some()).count(), 1);
    let id = outcomes[0].0.job.id;
    assert!(
        outcomes
            .iter()
            .all(|(s, c)| s.job.id == id && c.as_ref().is_none_or(|c| c.job().id == id))
    );
    let store = JobStore::open(&path).unwrap();
    for n in 0..8 {
        assert_eq!(
            store
                .find_request("verified-repository-a", &format!("request-{n}"))
                .unwrap()
                .unwrap()
                .id,
            id
        );
    }
}

#[test]
fn claims_cannot_cross_databases_with_identical_ids_and_worker_names() {
    let tmp = tempfile::tempdir().unwrap();
    let mut first = JobStore::open(&tmp.path().join("first.db")).unwrap();
    let mut second = JobStore::open(&tmp.path().join("second.db")).unwrap();
    first.submit(&request("same-key"), 1).unwrap();
    second.submit(&request("same-key"), 1).unwrap();
    let first_claim = first.claim_next("same-worker", 2).unwrap().unwrap();
    let second_claim = second.claim_next("same-worker", 2).unwrap().unwrap();
    assert_eq!(first_claim.job().id, second_claim.job().id);
    assert!(matches!(
        second.complete(&first_claim, &JobCompletion::Cancelled, 3),
        Err(JobError::ClaimLost(_))
    ));
    assert_eq!(
        second
            .complete(&second_claim, &JobCompletion::Cancelled, 3)
            .unwrap()
            .state,
        JobState::Cancelled
    );
    assert_eq!(
        first.get(first_claim.job().id).unwrap().state,
        JobState::Running
    );
}

#[test]
fn queue_deadlines_expire_without_claiming_and_cancellation_is_honest() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(&tmp.path().join("jobs.db")).unwrap();
    let expired = store.submit(&request("expired"), 1).unwrap().job;
    assert!(store.claim_next("worker", 1000).unwrap().is_none());
    assert_eq!(store.get(expired.id).unwrap().state, JobState::Expired);
    let mut queued = request("queued");
    queued.deadline_ms = 3000;
    let queued = store.submit(&queued, 1001).unwrap().job;
    let cancelled = store.request_cancel(queued.id, 1002).unwrap();
    assert_eq!(cancelled.state, JobState::Cancelled);
    assert!(cancelled.cancellation_requested);
    assert!(store.claim_next("worker", 1003).unwrap().is_none());
    let mut running = request("running");
    running.deadline_ms = 3000;
    let running = store.submit(&running, 1004).unwrap().job;
    let claim = store.claim_next("worker", 1005).unwrap().unwrap();
    assert_eq!(claim.job().id, running.id);
    let pending = store.request_cancel(running.id, 1006).unwrap();
    assert_eq!(pending.state, JobState::Running);
    assert!(pending.cancellation_requested);
    // A deadline or a cancellation request does not revoke a live worker's
    // ability to record what actually happened.
    let done = store
        .complete(
            &claim,
            &JobCompletion::Succeeded {
                result: serde_json::json!({}),
            },
            4000,
        )
        .unwrap();
    assert_eq!(done.state, JobState::Succeeded);
    assert!(done.cancellation_requested);
    assert_eq!(store.request_cancel(done.id, 5000).unwrap(), done);
}

#[test]
fn interrupted_work_remains_discoverable_and_never_retries_automatically() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    let mut store = JobStore::open(&path).unwrap();
    let first = store.submit(&request("original"), 1).unwrap().job;
    let claim = store.claim_next("old-generation", 2).unwrap().unwrap();
    assert_eq!(
        store.interrupt_worker("unrelated-generation", 3).unwrap(),
        0
    );
    assert_eq!(store.get(first.id).unwrap().state, JobState::Running);
    assert_eq!(store.interrupt_worker("old-generation", 4).unwrap(), 1);
    assert_eq!(store.interrupt_worker("old-generation", 5).unwrap(), 0);
    assert!(matches!(
        store.complete(&claim, &JobCompletion::Cancelled, 6),
        Err(JobError::ClaimLost(_))
    ));
    assert!(matches!(
        store.progress(
            &claim,
            &JobProgress {
                completed: 1,
                total: None,
                message: "stale".into()
            },
            6
        ),
        Err(JobError::ClaimLost(_))
    ));
    drop(store);
    let mut store = JobStore::open(&path).unwrap();
    assert!(store.claim_next("new-generation", 7).unwrap().is_none());
    let joined = store.submit(&request("retry-with-new-key"), 8).unwrap();
    assert!(joined.joined);
    assert_eq!(joined.job.id, first.id);
    assert_eq!(joined.job.state, JobState::Interrupted);
    assert_eq!(
        store.request_cancel(first.id, 9).unwrap().state,
        JobState::Interrupted
    );
}

#[test]
fn progress_is_shared_monotonic_and_completion_revokes_claim() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    let mut store = JobStore::open(&path).unwrap();
    let first = store.submit(&request("key"), 10).unwrap().job;
    let claim = store.claim_next("worker", 20).unwrap().unwrap();
    let progress = JobProgress {
        completed: 2,
        total: Some(5),
        message: "two checked".into(),
    };
    store.progress(&claim, &progress, 30).unwrap();
    let observer = JobStore::open(&path).unwrap();
    assert_eq!(
        observer.get(first.id).unwrap().progress,
        Some(progress.clone())
    );
    let mut backwards = progress.clone();
    backwards.completed = 1;
    assert!(matches!(
        store.progress(&claim, &backwards, 40),
        Err(JobError::Invalid(_))
    ));
    let mut invalid = progress.clone();
    invalid.total = Some(1);
    assert!(matches!(
        store.progress(&claim, &invalid, 40),
        Err(JobError::Invalid(_))
    ));
    let mut oversized = progress;
    oversized.message = "x".repeat(4097);
    assert!(matches!(
        store.progress(&claim, &oversized, 40),
        Err(JobError::Invalid(_))
    ));
    let failure = JobCompletion::Failed {
        message: "scan failed safely".into(),
    };
    // Clock regression must not move the persisted update timestamp backwards.
    let done = store.complete(&claim, &failure, 15).unwrap();
    assert_eq!(done.updated_at_ms, 30);
    assert_eq!(done.state, JobState::Failed);
    assert_eq!(done.completion, Some(failure));
    assert!(matches!(
        store.complete(&claim, &JobCompletion::Cancelled, 50),
        Err(JobError::ClaimLost(_))
    ));
}

#[test]
fn invalid_inputs_and_large_results_leave_the_job_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = JobStore::open(&tmp.path().join("jobs.db")).unwrap();
    let mut invalid = request("key");
    invalid.repository.clear();
    assert!(matches!(
        store.submit(&invalid, 1),
        Err(JobError::Invalid(_))
    ));
    invalid = request("\0");
    assert!(matches!(
        store.submit(&invalid, 1),
        Err(JobError::Invalid(_))
    ));
    assert!(matches!(
        store.submit(&request("past"), 1000),
        Err(JobError::Invalid(_))
    ));
    assert!(matches!(
        store.submit(&request("negative"), -1),
        Err(JobError::Invalid(_))
    ));
    let first = store.submit(&request("valid"), 1).unwrap().job;
    assert!(matches!(store.claim_next("", 2), Err(JobError::Invalid(_))));
    let claim = store.claim_next("worker", 2).unwrap().unwrap();
    let before = store.get(first.id).unwrap();
    let huge = JobCompletion::Succeeded {
        result: serde_json::json!("x".repeat(1024 * 1024)),
    };
    assert!(matches!(
        store.complete(&claim, &huge, 3),
        Err(JobError::Invalid(_))
    ));
    assert_eq!(store.get(first.id).unwrap(), before);
    assert_eq!(
        store
            .complete(&claim, &JobCompletion::Cancelled, 3)
            .unwrap()
            .state,
        JobState::Cancelled
    );
    assert!(matches!(store.get(-1), Err(JobError::NotFound(-1))));
    assert!(matches!(
        store.request_cancel(-1, 5),
        Err(JobError::NotFound(-1))
    ));
    assert!(
        store
            .find_request("verified-repository-a", "missing")
            .unwrap()
            .is_none()
    );
}

#[test]
fn foreign_and_future_databases_are_refused_without_schema_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let foreign = tmp.path().join("broker.db");
    let conn = rusqlite::Connection::open(&foreign).unwrap();
    conn.execute_batch("CREATE TABLE precious(value TEXT); INSERT INTO precious VALUES ('keep');")
        .unwrap();
    drop(conn);
    let before = std::fs::read(&foreign).unwrap();
    assert!(matches!(
        JobStore::open(&foreign),
        Err(JobError::IncompatibleDatabase)
    ));
    assert_eq!(std::fs::read(&foreign).unwrap(), before);
    let future = tmp.path().join("future.db");
    drop(JobStore::open(&future).unwrap());
    let conn = rusqlite::Connection::open(&future).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    drop(conn);
    assert!(matches!(
        JobStore::open(&future),
        Err(JobError::IncompatibleDatabase)
    ));
    let conn = rusqlite::Connection::open(&future).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn committed_claim_survives_a_process_exit_without_destructors() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "service_job_child_process_helper", "--nocapture"])
        .env("AETHYME_SERVICE_JOB_TEST_DATABASE", &path)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(23),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut store = JobStore::open(&path).unwrap();
    let job = store
        .find_request("verified-repository-a", "child-key")
        .unwrap()
        .unwrap();
    assert_eq!(job.state, JobState::Running);
    assert_eq!(job.progress.as_ref().unwrap().completed, 1);
    assert!(store.claim_next("replacement", 5).unwrap().is_none());
    // wait() above proves the exact child exited. Only then fence its claim.
    assert_eq!(store.interrupt_worker("exited-child", 6).unwrap(), 1);
    assert_eq!(store.get(job.id).unwrap().state, JobState::Interrupted);
}

#[test]
fn service_job_child_process_helper() {
    let Some(path) = std::env::var_os("AETHYME_SERVICE_JOB_TEST_DATABASE") else {
        return;
    };
    let mut store = JobStore::open(std::path::Path::new(&path)).unwrap();
    store.submit(&request("child-key"), 1).unwrap();
    let claim = store.claim_next("exited-child", 2).unwrap().unwrap();
    store
        .progress(
            &claim,
            &JobProgress {
                completed: 1,
                total: None,
                message: "committed before exit".into(),
            },
            3,
        )
        .unwrap();
    std::process::exit(23);
}
