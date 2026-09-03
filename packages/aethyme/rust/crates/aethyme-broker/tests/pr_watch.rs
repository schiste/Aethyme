use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use aethyme_broker::{
    Broker, BrokerOpError, DeliveryCompletion, DeliveryPolicy, DeliveryStatus, NewSession,
    PullRequestActivityKind, PullRequestActivityMetadata, PullRequestBatchAckOutcome,
    PullRequestBatchStatus, PullRequestSnapshot, PullRequestWatchError, PullRequestWatchProvider,
    PullRequestWatchRequest, PullRequestWatchStatus, SessionOrigin,
};

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn broker_fixture() -> (tempfile::TempDir, Broker, i64) {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(root.path().join("README.md"), "test\n").unwrap();
    git(root.path(), &["add", "README.md"]);
    git(root.path(), &["commit", "-qm", "initial"]);
    let mut broker = Broker::open(root.path()).unwrap();
    let session = broker
        .store()
        .register_session(&NewSession {
            worktree_path: root.path().display().to_string(),
            branch: "main".into(),
            origin: SessionOrigin::Adopted,
            task: Some("watch review".into()),
            diff_base: Some("a".repeat(40)),
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
        })
        .unwrap();
    (root, broker, session.id)
}

struct FakeProvider(Mutex<VecDeque<PullRequestSnapshot>>);

impl PullRequestWatchProvider for FakeProvider {
    fn inspect(
        &self,
        _request: &PullRequestWatchRequest,
    ) -> Result<PullRequestSnapshot, PullRequestWatchError> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| PullRequestWatchError::Provider("fixture exhausted".into()))
    }
}

struct ResultProvider(Mutex<VecDeque<Result<PullRequestSnapshot, PullRequestWatchError>>>);

impl PullRequestWatchProvider for ResultProvider {
    fn inspect(
        &self,
        _request: &PullRequestWatchRequest,
    ) -> Result<PullRequestSnapshot, PullRequestWatchError> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(PullRequestWatchError::Provider("fixture exhausted".into())))
    }
}

fn snapshot(state: &str, sha: char, activity: &[&str]) -> PullRequestSnapshot {
    PullRequestSnapshot {
        number: 7,
        title: "Review me".into(),
        url: "https://github.com/Owner/Repo/pull/7".into(),
        state: state.into(),
        target_branch: "main".into(),
        head_branch: "feature".into(),
        head_sha: sha.to_string().repeat(40),
        is_draft: true,
        activities: activity
            .iter()
            .map(|id| PullRequestActivityMetadata {
                kind: PullRequestActivityKind::Comment,
                provider_id: (*id).into(),
                author: Some("reviewer".into()),
                state: None,
                url: Some(format!("https://example.test/{id}")),
                updated_at: Some("2026-09-02T10:00:00Z".into()),
            })
            .collect(),
    }
}

#[test]
fn durable_watch_tracks_metadata_changes_and_completes_with_the_pr() {
    let (_root, mut broker, session_id) = broker_fixture();
    let provider = FakeProvider(Mutex::new(VecDeque::from([
        snapshot("open", 'a', &["C1"]),
        snapshot("open", 'b', &["C1", "C2"]),
        snapshot("merged", 'b', &["C1", "C2"]),
    ])));

    let watch = broker
        .start_pull_request_watch(
            session_id,
            "Owner/Repo",
            7,
            vec![PullRequestActivityKind::Comment],
            60,
            &provider,
            1_000,
        )
        .unwrap();
    assert_eq!(watch.canonical_repository, "github.com/owner/repo");
    assert_eq!(watch.status, PullRequestWatchStatus::Active);
    assert!(watch.is_draft);
    let subscription = broker
        .subscribe_pull_request_delivery(
            watch.id,
            "test-adapter",
            "recipient-1",
            DeliveryPolicy::Notify,
            2_000,
        )
        .unwrap();

    let changed = broker
        .poll_pull_request_watch(watch.id, &provider, 61_000)
        .unwrap();
    assert!(changed.changed);
    assert_eq!(changed.activity_count, 2);
    assert_eq!(changed.new_activity_count, 1);
    assert_eq!(changed.watch.head_sha, "b".repeat(40));
    let batch = changed.batch.expect("new C2 is batched");
    assert_eq!(batch.activities[0].metadata.provider_id, "C2");
    let claimed = broker
        .claim_next_delivery("test-adapter", "worker-1", 120, 61_100)
        .unwrap()
        .delivery
        .expect("new batch is enqueued for active subscription");
    assert_eq!(claimed.subscription, subscription);
    assert_eq!(claimed.batch, batch);
    assert!(claimed.prompt.contains("does not authorize a push"));
    assert!(!claimed.prompt.contains("untrusted secret"));
    let retried = broker
        .complete_delivery(
            claimed.item.id,
            "worker-1",
            claimed.item.generation,
            DeliveryCompletion::Retry,
            Some("recipient_busy"),
            61_200,
        )
        .unwrap();
    assert_eq!(retried.status, DeliveryStatus::Pending);
    let claimed_again = broker
        .claim_next_delivery("test-adapter", "worker-2", 120, 61_300)
        .unwrap()
        .delivery
        .unwrap();
    assert_eq!(claimed_again.item.generation, claimed.item.generation + 1);
    assert!(matches!(
        broker
            .complete_delivery(
                claimed.item.id,
                "worker-1",
                claimed.item.generation,
                DeliveryCompletion::Delivered,
                None,
                61_400,
            )
            .unwrap_err(),
        BrokerOpError::Store(aethyme_broker::BrokerError::DeliveryClaimChanged { .. })
    ));
    let delivered = broker
        .complete_delivery(
            claimed_again.item.id,
            "worker-2",
            claimed_again.item.generation,
            DeliveryCompletion::Delivered,
            None,
            61_500,
        )
        .unwrap();
    assert_eq!(delivered.status, DeliveryStatus::Delivered);
    assert_eq!(
        broker
            .pull_request_activity_batches(watch.id, false)
            .unwrap(),
        vec![batch.clone()]
    );

    let acknowledged = broker
        .acknowledge_pull_request_activity_batch(
            batch.id,
            PullRequestBatchAckOutcome::Addressed,
            "fixed in the current PR head",
            62_000,
        )
        .unwrap();
    assert_eq!(acknowledged.status, PullRequestBatchStatus::Acknowledged);
    assert_eq!(acknowledged.ack_reason_digest.as_deref().unwrap().len(), 64);
    assert_eq!(
        broker
            .acknowledge_pull_request_activity_batch(
                batch.id,
                PullRequestBatchAckOutcome::Addressed,
                "fixed in the current PR head",
                63_000,
            )
            .unwrap(),
        acknowledged
    );
    assert!(matches!(
        broker
            .acknowledge_pull_request_activity_batch(
                batch.id,
                PullRequestBatchAckOutcome::Stale,
                "different classification",
                64_000,
            )
            .unwrap_err(),
        BrokerOpError::Store(aethyme_broker::BrokerError::PullRequestActivityBatchAckConflict(_))
    ));
    assert_eq!(
        broker
            .pull_request_activity_batches(watch.id, false)
            .unwrap(),
        Vec::new()
    );

    let completed = broker
        .poll_pull_request_watch(watch.id, &provider, 121_000)
        .unwrap();
    assert_eq!(completed.watch.status, PullRequestWatchStatus::Completed);
    assert_eq!(completed.new_activity_count, 0);
    assert!(completed.batch.is_none());
    assert_eq!(broker.pull_request_watches(false).unwrap().len(), 0);
    assert_eq!(broker.pull_request_watches(true).unwrap().len(), 1);
}

#[test]
fn duplicate_live_watch_is_refused_for_case_equivalent_repository() {
    let (_root, mut broker, session_id) = broker_fixture();
    let provider = FakeProvider(Mutex::new(VecDeque::from([
        snapshot("open", 'a', &[]),
        snapshot("open", 'a', &[]),
    ])));
    broker
        .start_pull_request_watch(
            session_id,
            "Owner/Repo",
            7,
            vec![PullRequestActivityKind::Review],
            60,
            &provider,
            1_000,
        )
        .unwrap();
    let error = broker
        .start_pull_request_watch(
            session_id,
            "owner/repo",
            7,
            vec![PullRequestActivityKind::Review],
            60,
            &provider,
            2_000,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        BrokerOpError::Store(aethyme_broker::BrokerError::PullRequestWatchIdentityConflict { .. })
    ));
}

#[test]
fn scheduler_tick_shares_rate_limit_backoff_without_busy_polling() {
    let (_root, mut broker, session_id) = broker_fixture();
    let provider = ResultProvider(Mutex::new(VecDeque::from([
        Ok(snapshot("open", 'a', &[])),
        Ok(snapshot("open", 'b', &[])),
        Ok(snapshot("open", 'c', &[])),
        Err(PullRequestWatchError::Provider(
            "API rate limit exceeded for this account".into(),
        )),
        Ok(snapshot("open", 'd', &["must-not-be-polled"])),
    ])));
    for repository in ["Owner/Alpha", "Owner/Beta", "Owner/Gamma"] {
        broker
            .start_pull_request_watch(
                session_id,
                repository,
                7,
                vec![PullRequestActivityKind::Comment],
                15,
                &provider,
                1_000,
            )
            .unwrap();
    }

    let report = broker
        .tick_pull_request_watches(&provider, 16_000, 10)
        .unwrap();
    assert_eq!(report.schema_version, 1);
    assert_eq!(report.due_watch_count, 3);
    assert_eq!(report.attempted_watch_count, 1);
    assert_eq!(report.failed_watch_count, 1);
    assert_eq!(report.deferred_watch_count, 2);
    assert_eq!(report.rate_limit_until, Some(916_000));
    assert_eq!(report.next_tick_at, Some(916_000));
    assert_eq!(
        report.results[0].error_code.as_deref(),
        Some("rate_limited")
    );
    assert_eq!(
        report.results[1].error_code.as_deref(),
        Some("rate_limited_shared")
    );
    assert!(!report.results[1].attempted);
    assert_eq!(
        broker
            .tick_pull_request_watches(&provider, 16_001, 10)
            .unwrap()
            .due_watch_count,
        0
    );
    let deferred = broker
        .pull_request_watch(report.results[1].watch_id)
        .unwrap();
    assert_eq!(deferred.last_polled_at, None);

    // Only the first tick provider call was consumed; deferred watches did
    // not contact GitHub after shared rate-limit evidence was observed.
    assert_eq!(provider.0.lock().unwrap().len(), 1);
}

#[test]
fn scheduler_tick_is_bounded_deterministic_and_persists_safe_error_codes() {
    let (_root, mut broker, session_id) = broker_fixture();
    let provider = ResultProvider(Mutex::new(VecDeque::from([
        Ok(snapshot("open", 'a', &[])),
        Ok(snapshot("open", 'b', &[])),
        Err(PullRequestWatchError::Provider(
            "authentication token is invalid: secret detail".into(),
        )),
    ])));
    for repository in ["Owner/Zulu", "Owner/Alpha"] {
        broker
            .start_pull_request_watch(
                session_id,
                repository,
                7,
                vec![PullRequestActivityKind::Review],
                15,
                &provider,
                1_000,
            )
            .unwrap();
    }

    let report = broker
        .tick_pull_request_watches(&provider, 16_000, 1)
        .unwrap();
    assert_eq!(report.due_watch_count, 2);
    assert_eq!(report.results.len(), 1);
    assert_eq!(
        report.results[0].canonical_repository,
        "github.com/owner/alpha"
    );
    assert_eq!(
        report.results[0].error_code.as_deref(),
        Some("authentication_failed")
    );
    assert_eq!(report.results[0].retry_at, Some(316_000));
    assert_eq!(report.next_tick_at, Some(16_000));

    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("secret detail"));
    assert!(
        broker
            .tick_pull_request_watches(&provider, 16_000, 0)
            .is_err()
    );
}
