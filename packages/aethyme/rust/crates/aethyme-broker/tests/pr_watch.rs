use std::collections::VecDeque;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use aethyme_broker::{
    Broker, BrokerOpError, NewSession, PullRequestActivityKind, PullRequestActivityMetadata,
    PullRequestBatchAckOutcome, PullRequestBatchStatus, PullRequestSnapshot, PullRequestWatchError,
    PullRequestWatchProvider, PullRequestWatchRequest, PullRequestWatchStatus, SessionOrigin,
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

    let changed = broker
        .poll_pull_request_watch(watch.id, &provider, 61_000)
        .unwrap();
    assert!(changed.changed);
    assert_eq!(changed.activity_count, 2);
    assert_eq!(changed.new_activity_count, 1);
    assert_eq!(changed.watch.head_sha, "b".repeat(40));
    let batch = changed.batch.expect("new C2 is batched");
    assert_eq!(batch.activities[0].metadata.provider_id, "C2");
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
