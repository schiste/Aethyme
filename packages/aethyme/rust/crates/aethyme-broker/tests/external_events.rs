use std::path::Path;
use std::process::Command;

use aethyme_broker::{
    Broker, ExternalEventEnvelope, ExternalEventProvider, ExternalEventReconciliation,
    ExternalEventStatus, ExternalVerificationMethod, NewPrWatchState, SessionStatus,
    VerifiedExternalSource, external_event_digest,
};

const NOW: i64 = 2_000_000_000_000;

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
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "initial\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "initial"]);
    git(
        tmp.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://adapter:credential@github.com/acme/product.git",
        ],
    );
    tmp
}

fn worktree(root: &Path, name: &str) -> std::path::PathBuf {
    let path = root.join(".aethyme/worktrees").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    git(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("agent/{name}"),
            path.to_str().unwrap(),
            "main",
        ],
    );
    path
}

fn envelope(id: &str, event_type: &str, pr_number: i64, commit: &str) -> ExternalEventEnvelope {
    let mut event = ExternalEventEnvelope {
        schema_version: 1,
        provider: ExternalEventProvider::Github,
        provider_event_id: id.into(),
        event_type: event_type.into(),
        repository: "github.com/acme/product".into(),
        target_branch: "main".into(),
        pr_number,
        commit_sha: commit.into(),
        occurred_at: NOW - 1_000,
        verified_source: VerifiedExternalSource {
            method: ExternalVerificationMethod::WebhookSignature,
            verified_at: NOW - 500,
        },
        normalized_digest: "0".repeat(64),
    };
    event.normalized_digest = external_event_digest(&event);
    event
}

fn known_pr(broker: &mut Broker, pr_number: i64, session_id: Option<i64>) {
    broker
        .store()
        .upsert_pr_watch_state(&NewPrWatchState {
            target_branch: "main".into(),
            pr_number,
            activity_fingerprint: "known".into(),
            marker: "none".into(),
            last_dispatch_at: None,
            last_agent_session_id: session_id,
        })
        .unwrap();
}

#[test]
fn allowlisted_events_create_non_blocking_advisories_and_redelivery_is_idempotent() {
    let tmp = fixture();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = worktree(tmp.path(), "SECRET-host-path");
    let session = broker
        .adopt(&wt, Some("SECRET task text and comment body"))
        .unwrap();
    std::fs::write(wt.join("README.md"), "owned\n").unwrap();
    git(&wt, &["add", "README.md"]);
    git(&wt, &["commit", "-qm", "owned"]);
    let owned_commit = git(&wt, &["rev-parse", "HEAD"]);
    let base = git(tmp.path(), &["rev-parse", "main"]);
    let queue = broker
        .store()
        .submit(session.id, &owned_commit, &base)
        .unwrap();
    known_pr(&mut broker, 42, Some(session.id));

    let kinds = [
        "review_changes_requested",
        "review_approved",
        "queue_ejected",
        "validation_failed",
    ];
    let mut advisory_ids = Vec::new();
    for (index, kind) in kinds.iter().enumerate() {
        let report = broker
            .ingest_external_event(
                envelope(&format!("delivery-{index}"), kind, 42, &owned_commit),
                NOW,
            )
            .unwrap();
        assert_eq!(report.event.status, ExternalEventStatus::AdvisoryCreated);
        assert_eq!(report.event.session_id, Some(session.id));
        assert_eq!(report.event.queue_entry_id, Some(queue.id));
        assert!(report.non_blocking);
        assert!(!report.deduplicated);
        advisory_ids.push(report.advisory.unwrap().id);
    }
    let event_count = broker.store().external_events(true).unwrap().len();
    let journal_count = broker.store().events_after(0, i64::MAX).unwrap().len();
    let duplicate = broker
        .ingest_external_event(envelope("delivery-0", kinds[0], 42, &owned_commit), NOW + 1)
        .unwrap();
    assert!(duplicate.deduplicated);
    assert_eq!(duplicate.advisory.unwrap().id, advisory_ids[0]);
    assert_eq!(
        broker.store().external_events(true).unwrap().len(),
        event_count
    );
    assert_eq!(
        broker.store().events_after(0, i64::MAX).unwrap().len(),
        journal_count
    );

    let persisted = serde_json::to_string(&broker.store().external_events(true).unwrap()).unwrap();
    let advisories = serde_json::to_string(&broker.store().advisories(true).unwrap()).unwrap();
    for forbidden in [
        "SECRET task text",
        "SECRET-host-path",
        "adapter",
        "credential",
        wt.to_str().unwrap(),
        "comment body",
        "diff --git",
    ] {
        assert!(
            !persisted.contains(forbidden),
            "event storage leaked {forbidden:?}"
        );
        assert!(
            !advisories.contains(forbidden),
            "advisory leaked {forbidden:?}"
        );
    }
    let projection =
        std::fs::read_to_string(tmp.path().join(".aethyme/broker-advisory.md")).unwrap();
    assert!(projection.contains("external_event_type"));
}

#[test]
fn ownership_resolution_handles_rewrites_closure_ambiguity_unknown_prs_and_staleness() {
    let tmp = fixture();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let first_wt = worktree(tmp.path(), "first");
    let second_wt = worktree(tmp.path(), "second");
    let first = broker.adopt(&first_wt, None).unwrap();
    let second = broker.adopt(&second_wt, None).unwrap();
    std::fs::write(first_wt.join("README.md"), "first\n").unwrap();
    git(&first_wt, &["add", "README.md"]);
    git(&first_wt, &["commit", "-qm", "first"]);
    let old_commit = git(&first_wt, &["rev-parse", "HEAD"]);
    let base = git(tmp.path(), &["rev-parse", "main"]);
    broker.store().submit(first.id, &old_commit, &base).unwrap();
    std::fs::write(first_wt.join("README.md"), "rewritten followup\n").unwrap();
    git(&first_wt, &["add", "README.md"]);
    git(&first_wt, &["commit", "-qm", "followup"]);
    broker
        .store()
        .set_session_status(first.id, SessionStatus::Closed, None)
        .unwrap();
    known_pr(&mut broker, 10, Some(first.id));
    let rewritten = broker
        .ingest_external_event(
            envelope("rewritten", "validation_failed", 10, &old_commit),
            NOW,
        )
        .unwrap();
    assert_eq!(rewritten.event.session_id, Some(first.id));
    assert_eq!(rewritten.event.status, ExternalEventStatus::AdvisoryCreated);

    let unknown_pr = broker
        .ingest_external_event(
            envelope("unknown-pr", "review_changes_requested", 999, &old_commit),
            NOW,
        )
        .unwrap();
    assert_eq!(
        unknown_pr.event.status,
        ExternalEventStatus::UnknownPullRequest
    );
    assert!(unknown_pr.advisory.is_none());

    let mut stale = envelope("stale", "queue_ejected", 10, &old_commit);
    stale.occurred_at = NOW - aethyme_broker::EXTERNAL_EVENT_MAX_AGE_MS - 1;
    stale.normalized_digest = external_event_digest(&stale);
    let stale = broker.ingest_external_event(stale, NOW).unwrap();
    assert_eq!(stale.event.status, ExternalEventStatus::Stale);

    broker
        .store()
        .submit(second.id, &old_commit, &base)
        .unwrap();
    known_pr(&mut broker, 11, None);
    let ambiguous = broker
        .ingest_external_event(
            envelope("ambiguous", "review_approved", 11, &old_commit),
            NOW,
        )
        .unwrap();
    assert_eq!(ambiguous.event.status, ExternalEventStatus::AmbiguousOwner);
    assert_eq!(ambiguous.ownership_candidates.len(), 2);

    let unknown_type = broker
        .ingest_external_event(envelope("future", "review_maybe", 10, &old_commit), NOW)
        .unwrap();
    assert_eq!(
        unknown_type.event.status,
        ExternalEventStatus::UnknownEventType
    );
}

#[test]
fn unresolved_events_require_explicit_redacted_reconciliation_and_strict_inputs() {
    let tmp = fixture();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = worktree(tmp.path(), "operator");
    let session = broker.adopt(&wt, None).unwrap();
    let unknown_commit = "a".repeat(40);
    known_pr(&mut broker, 55, None);
    let unresolved = broker
        .ingest_external_event(
            envelope("needs-owner", "validation_failed", 55, &unknown_commit),
            NOW,
        )
        .unwrap();
    assert_eq!(unresolved.event.status, ExternalEventStatus::OwnerNotFound);
    let assigned = broker
        .reconcile_external_event(
            unresolved.event.id,
            ExternalEventReconciliation::Assign {
                session_id: session.id,
            },
            "SECRET operator explanation",
            NOW + 1,
        )
        .unwrap();
    assert_eq!(assigned.event.status, ExternalEventStatus::AdvisoryCreated);
    assert_eq!(assigned.event.session_id, Some(session.id));
    assert_eq!(
        assigned.event.reconciliation_kind.as_deref(),
        Some("assigned")
    );
    assert_eq!(
        assigned
            .event
            .reconciliation_reason_digest
            .as_deref()
            .unwrap()
            .len(),
        64
    );
    assert!(!serde_json::to_string(&assigned).unwrap().contains("SECRET"));

    let unknown = broker
        .ingest_external_event(
            envelope("unknown-kind", "not_supported", 55, &unknown_commit),
            NOW,
        )
        .unwrap();
    assert!(
        broker
            .reconcile_external_event(
                unknown.event.id,
                ExternalEventReconciliation::Assign {
                    session_id: session.id,
                },
                "assign unknown",
                NOW + 2,
            )
            .is_err()
    );
    let ignored = broker
        .reconcile_external_event(
            unknown.event.id,
            ExternalEventReconciliation::Ignore,
            "reviewed unsupported provider event",
            NOW + 3,
        )
        .unwrap();
    assert_eq!(ignored.event.status, ExternalEventStatus::Ignored);

    let mut conflicting = envelope("needs-owner", "review_approved", 55, &unknown_commit);
    conflicting.normalized_digest = external_event_digest(&conflicting);
    assert!(broker.ingest_external_event(conflicting, NOW + 4).is_err());

    let smuggled = serde_json::json!({
        "schema_version": 1,
        "provider": "github",
        "provider_event_id": "delivery",
        "event_type": "validation_failed",
        "repository": "github.com/acme/product",
        "target_branch": "main",
        "pr_number": 1,
        "commit_sha": "a".repeat(40),
        "occurred_at": NOW,
        "verified_source": {"method": "webhook_signature", "verified_at": NOW},
        "normalized_digest": "a".repeat(64),
        "webhook_body": "SECRET raw payload"
    });
    assert!(serde_json::from_value::<ExternalEventEnvelope>(smuggled).is_err());
}
