//! Round-trip tests for every table plus contract-level behaviors
//! (issue #4 acceptance: schema applies fresh, per-table round trips;
//! issue #5: migrations from empty and from v1).

use aethyme_broker::{
    BrokerError, BrokerStore, GateDef, GateFailureClass, GateStatus, LeaseKind, MergeStatus,
    NewGateResult, NewPrWatchState, NewSession, RepositoryContract, SessionOrigin, SessionStatus,
};

fn open_temp() -> (tempfile::TempDir, BrokerStore) {
    let tmp = tempfile::tempdir().unwrap();
    let store = BrokerStore::open_in_repo(tmp.path()).unwrap();
    (tmp, store)
}

fn sample_session(store: &mut BrokerStore) -> aethyme_broker::Session {
    store
        .register_session(&NewSession {
            worktree_path: "/repo/.aethyme/worktrees/fix-auth".into(),
            branch: "agent/fix-auth".into(),
            origin: SessionOrigin::Adopted,
            task: Some("Fix auth bug".into()),
            diff_base: Some("abc123".into()),
            adoption_base: None,
            adopted_head: None,
            repository_contract: Some(RepositoryContract {
                repository_schema: Some(1),
                deployment_state_digest: "deployment-digest".into(),
                aethyme_version: "0.2.2".into(),
                gate_definition_digest: Some("gate-digest".into()),
                backfilled: false,
            }),
            pid: None,
            command: None,
            log_path: None,
        })
        .unwrap()
}

#[test]
fn opens_in_repo_at_documented_path_and_reopens() {
    let tmp = tempfile::tempdir().unwrap();
    let store = BrokerStore::open_in_repo(tmp.path()).unwrap();
    assert!(store.db_path().ends_with(aethyme_broker::BROKER_DB_RELPATH));
    drop(store);
    // Reopen: migration is idempotent.
    BrokerStore::open_in_repo(tmp.path()).unwrap();
}

#[test]
fn session_round_trip_attach_first() {
    let (_tmp, mut store) = open_temp();
    let session = sample_session(&mut store);

    // Attach-first: no pid/command is a fully supported state.
    assert_eq!(session.origin, SessionOrigin::Adopted);
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.pid, None);

    let fetched = store.session(session.id).unwrap();
    assert_eq!(fetched.worktree_path, session.worktree_path);
    assert_eq!(fetched.task.as_deref(), Some("Fix auth bug"));
    assert_eq!(fetched.adoption_base.as_deref(), Some("abc123"));
    assert_eq!(fetched.adopted_head.as_deref(), Some("abc123"));
    assert_eq!(fetched.accepted_session_head, None);
    assert_eq!(fetched.repository_contract, session.repository_contract);

    store.touch_session_activity(session.id, 42_000).unwrap();
    assert_eq!(store.session(session.id).unwrap().last_activity_at, 42_000);

    store
        .set_session_status(session.id, SessionStatus::Exited, Some(1))
        .unwrap();
    let exited = store.session(session.id).unwrap();
    assert_eq!(exited.status, SessionStatus::Exited);
    assert_eq!(exited.exit_code, Some(1));
}

#[test]
fn duplicate_live_worktree_is_rejected_but_cleaned_frees_the_slot() {
    let (_tmp, mut store) = open_temp();
    let session = sample_session(&mut store);

    let duplicate = store.register_session(&NewSession {
        worktree_path: session.worktree_path.clone(),
        branch: "agent/other".into(),
        origin: SessionOrigin::Spawned,
        task: None,
        diff_base: None,
        adoption_base: None,
        adopted_head: None,
        repository_contract: None,
        pid: Some(123),
        command: Some("claude".into()),
        log_path: None,
    });
    assert!(matches!(
        duplicate,
        Err(BrokerError::WorktreeAlreadyRegistered(_))
    ));

    // History rows (cleaned) do not block re-registration of the path.
    store
        .set_session_status(session.id, SessionStatus::Cleaned, None)
        .unwrap();
    store
        .register_session(&NewSession {
            worktree_path: session.worktree_path.clone(),
            branch: "agent/second-run".into(),
            origin: SessionOrigin::Adopted,
            task: None,
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
        })
        .unwrap();
}

#[test]
fn pr_watch_state_round_trip_and_upsert() {
    let (_tmp, mut store) = open_temp();

    let first = store
        .upsert_pr_watch_state(&NewPrWatchState {
            target_branch: "production".into(),
            pr_number: 42,
            activity_fingerprint: "initial".into(),
            marker: "none".into(),
            last_dispatch_at: None,
            last_agent_session_id: None,
        })
        .unwrap();
    assert_eq!(first.target_branch, "production");
    assert_eq!(first.pr_number, 42);
    assert_eq!(first.activity_fingerprint, "initial");
    assert_eq!(first.marker, "none");

    let second = store
        .upsert_pr_watch_state(&NewPrWatchState {
            target_branch: "production".into(),
            pr_number: 42,
            activity_fingerprint: "changed".into(),
            marker: "looking".into(),
            last_dispatch_at: Some(123_000),
            last_agent_session_id: Some(7),
        })
        .unwrap();
    assert_eq!(second.id, first.id);
    assert_eq!(second.activity_fingerprint, "changed");
    assert_eq!(second.marker, "looking");
    assert_eq!(second.last_dispatch_at, Some(123_000));
    assert_eq!(second.last_agent_session_id, Some(7));

    let fetched = store.pr_watch_state("production", 42).unwrap().unwrap();
    assert_eq!(fetched.id, first.id);
    assert_eq!(fetched.activity_fingerprint, "changed");
}

#[test]
fn lease_round_trip_expiry_and_activity_filter() {
    let (_tmp, mut store) = open_temp();
    let session = sample_session(&mut store);

    store
        .set_implicit_leases(session.id, &["src/auth.py".into(), "src/main.py".into()])
        .unwrap();
    let claimed = store
        .claim_lease(session.id, "src/auth/", Some(60_000))
        .unwrap();
    assert_eq!(claimed.kind, LeaseKind::Explicit);
    assert!(claimed.expires_at.is_some());

    let active = store.active_leases().unwrap();
    assert_eq!(active.len(), 3);

    // Implicit leases are replaced wholesale, explicit untouched.
    store
        .set_implicit_leases(session.id, &["src/other.py".into()])
        .unwrap();
    let active = store.active_leases().unwrap();
    assert_eq!(active.len(), 2);

    // Expired explicit lease drops out without any daemon (checked on read).
    let expired = store.claim_lease(session.id, "docs/", Some(-1)).unwrap();
    assert!(expired.expires_at.unwrap() <= expired.created_at);
    assert_eq!(store.active_leases().unwrap().len(), 2);

    // Released lease drops out.
    store.release_lease(session.id, "src/auth/").unwrap();
    assert_eq!(store.active_leases().unwrap().len(), 1);

    // Exited sessions hold no active leases.
    store
        .set_session_status(session.id, SessionStatus::Exited, Some(0))
        .unwrap();
    assert_eq!(store.active_leases().unwrap().len(), 0);
}

#[test]
fn cleaning_a_session_purges_its_lease_rows() {
    let (_tmp, mut store) = open_temp();
    let session = sample_session(&mut store);

    store
        .set_implicit_leases(session.id, &["src/auth.py".into(), "src/main.py".into()])
        .unwrap();
    store.claim_lease(session.id, "docs/", None).unwrap();
    assert_eq!(store.session_leases(session.id).unwrap().len(), 3);

    // Exited is NOT terminal (adopt --reuse can revive it): rows stay.
    store
        .set_session_status(session.id, SessionStatus::Exited, Some(0))
        .unwrap();
    assert_eq!(store.session_leases(session.id).unwrap().len(), 3);

    // Cleaned IS terminal: rows are purged in the same transaction —
    // regression for the 722 orphaned rows / ~25 sessions the 2026-07-17
    // dogfood database accumulated.
    store
        .set_session_status(session.id, SessionStatus::Cleaned, None)
        .unwrap();
    assert!(store.session_leases(session.id).unwrap().is_empty());
}

#[test]
fn retention_sweep_drops_leases_of_already_cleaned_sessions() {
    let (_tmp, mut store) = open_temp();
    let kept = sample_session(&mut store);
    store
        .set_implicit_leases(kept.id, &["src/live.py".into()])
        .unwrap();

    let cleaned = store
        .register_session(&NewSession {
            worktree_path: "/repo/.aethyme/worktrees/other".into(),
            branch: "agent/other".into(),
            origin: SessionOrigin::Adopted,
            task: None,
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
        })
        .unwrap();
    store
        .set_session_status(cleaned.id, SessionStatus::Cleaned, None)
        .unwrap();
    // Plant rows AFTER the clean — set_implicit_leases does not gate on
    // session status, which is exactly how pre-purge databases (and any
    // racing refresh) end up with orphaned rows.
    store
        .set_implicit_leases(cleaned.id, &["src/old.py".into(), "src/gone.py".into()])
        .unwrap();
    assert_eq!(store.session_leases(cleaned.id).unwrap().len(), 2);

    assert_eq!(store.purge_leases_of_cleaned_sessions().unwrap(), 2);
    assert!(store.session_leases(cleaned.id).unwrap().is_empty());
    // Live sessions' rows are untouched.
    assert_eq!(store.session_leases(kept.id).unwrap().len(), 1);
}

#[test]
fn gate_result_cache_ignores_cancelled_and_error_runs() {
    let (_tmp, mut store) = open_temp();
    store
        .upsert_gate(&GateDef {
            name: "pytest".into(),
            command: "pytest -q tests/local".into(),
            cost_tier: 2,
            triggers_json: "[\"**/*.py\"]".into(),
            resources_json: "[]".into(),
            resource_ttl_seconds: 300,
            definition_hash: "test-definition".into(),
            updated_at: 0,
        })
        .unwrap();
    assert_eq!(store.gates().unwrap().len(), 1);

    assert!(
        store
            .cached_gate_result("pytest", "tree-a")
            .unwrap()
            .is_none()
    );

    store
        .record_gate_result(&NewGateResult {
            gate_name: "pytest".into(),
            tree_hash: "tree-a".into(),
            definition_hash: "test-definition".into(),
            status: GateStatus::Cancelled,
            failure_class: None,
            exit_code: None,
            duration_ms: None,
            log_path: None,
            session_id: None,
        })
        .unwrap();
    assert!(
        store
            .cached_gate_result("pytest", "tree-a")
            .unwrap()
            .is_none(),
        "cancelled runs must not satisfy the cache"
    );

    store
        .record_gate_result(&NewGateResult {
            gate_name: "pytest".into(),
            tree_hash: "tree-a".into(),
            definition_hash: "test-definition".into(),
            status: GateStatus::Pass,
            failure_class: None,
            exit_code: Some(0),
            duration_ms: Some(1200),
            log_path: None,
            session_id: None,
        })
        .unwrap();
    let hit = store
        .cached_gate_result("pytest", "tree-a")
        .unwrap()
        .unwrap();
    assert_eq!(hit.status, GateStatus::Pass);
    assert_eq!(hit.failure_class, None);
    assert!(
        store
            .cached_gate_result_for_definition("pytest", "tree-a", "test-definition")
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .cached_gate_result_for_definition("pytest", "tree-a", "changed-definition")
            .unwrap()
            .is_none(),
        "a changed gate definition must invalidate cached proof"
    );

    store
        .record_gate_result(&NewGateResult {
            gate_name: "pytest".into(),
            tree_hash: "tree-c".into(),
            definition_hash: "test-definition".into(),
            status: GateStatus::Fail,
            failure_class: None,
            exit_code: Some(1),
            duration_ms: Some(100),
            log_path: None,
            session_id: None,
        })
        .unwrap();
    assert!(
        store
            .cached_gate_result("pytest", "tree-c")
            .unwrap()
            .is_none(),
        "legacy/unclassified fail rows must not satisfy the cache"
    );

    store
        .record_gate_result(&NewGateResult {
            gate_name: "pytest".into(),
            tree_hash: "tree-c".into(),
            definition_hash: "test-definition".into(),
            status: GateStatus::Fail,
            failure_class: Some(GateFailureClass::TestFailure),
            exit_code: Some(1),
            duration_ms: Some(100),
            log_path: None,
            session_id: None,
        })
        .unwrap();
    let hit = store
        .cached_gate_result("pytest", "tree-c")
        .unwrap()
        .unwrap();
    assert_eq!(hit.status, GateStatus::Fail);
    assert_eq!(hit.failure_class, Some(GateFailureClass::TestFailure));

    // Different tree = different cache slot.
    assert!(
        store
            .cached_gate_result("pytest", "tree-b")
            .unwrap()
            .is_none()
    );
}

#[test]
fn merge_queue_submit_is_idempotent_and_transitions_emit_events() {
    let (_tmp, mut store) = open_temp();
    let session = sample_session(&mut store);

    let first = store.submit(session.id, "head1", "base1").unwrap();
    let again = store.submit(session.id, "head1", "base1").unwrap();
    assert_eq!(first.id, again.id, "submit must be idempotent per head");
    assert_eq!(first.status, MergeStatus::Submitted);

    store
        .set_merge_status(
            first.id,
            MergeStatus::Verified,
            Some("mergedtree1"),
            Some("{\"gates\":\"all pass\"}"),
        )
        .unwrap();
    let queue = store.merge_queue().unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].status, MergeStatus::Verified);
    assert_eq!(queue[0].merged_tree.as_deref(), Some("mergedtree1"));

    let kinds: Vec<String> = store
        .events_after(0, i64::MAX)
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert!(kinds.contains(&"merge.submitted".to_string()));
    assert!(kinds.contains(&"merge.verified".to_string()));
    // Idempotent resubmit emitted no second submitted event.
    assert_eq!(kinds.iter().filter(|k| *k == "merge.submitted").count(), 1);
}

#[test]
fn promotion_atomically_advances_the_contribution_checkpoint() {
    let (_tmp, mut store) = open_temp();
    let session = sample_session(&mut store);
    let entry = store
        .submit(session.id, "session-head-1", "integration-base")
        .unwrap();
    store
        .set_merge_status(
            entry.id,
            MergeStatus::Verified,
            Some("integration-tree-1"),
            Some("{\"merge_commit\":\"candidate\"}"),
        )
        .unwrap();

    store
        .record_merge_promotion(
            entry.id,
            "integration-commit-1",
            "{\"commit\":\"integration-commit-1\"}",
        )
        .unwrap();

    let accepted = store.session(session.id).unwrap();
    assert_eq!(accepted.adopted_head.as_deref(), Some("abc123"));
    assert_eq!(
        accepted.accepted_session_head.as_deref(),
        Some("session-head-1")
    );
    assert_eq!(
        accepted.accepted_integration_commit.as_deref(),
        Some("integration-commit-1")
    );
    assert_eq!(
        accepted.accepted_integration_tree.as_deref(),
        Some("integration-tree-1")
    );
    assert_eq!(accepted.accepted_queue_entry_id, Some(entry.id));
    assert!(accepted.accepted_at.is_some());
    assert_eq!(
        store.merge_queue().unwrap()[0].status,
        MergeStatus::Promoted
    );
}

#[test]
fn content_empty_supersession_atomically_advances_the_contribution_checkpoint() {
    let (_tmp, mut store) = open_temp();
    let session = sample_session(&mut store);
    let entry = store
        .submit(session.id, "session-head-empty", "integration-base")
        .unwrap();
    store
        .set_merge_status(entry.id, MergeStatus::Simulating, None, None)
        .unwrap();

    store
        .record_content_empty_supersession(
            entry.id,
            "integration-commit-current",
            "integration-tree-current",
            "{\"reason\":\"submission produces no content change\"}",
        )
        .unwrap();

    let accepted = store.session(session.id).unwrap();
    assert_eq!(
        accepted.accepted_session_head.as_deref(),
        Some("session-head-empty")
    );
    assert_eq!(
        accepted.accepted_integration_commit.as_deref(),
        Some("integration-commit-current")
    );
    assert_eq!(
        accepted.accepted_integration_tree.as_deref(),
        Some("integration-tree-current")
    );
    assert_eq!(accepted.accepted_queue_entry_id, Some(entry.id));
    assert!(accepted.accepted_at.is_some());
    assert_eq!(
        store.merge_queue().unwrap()[0].status,
        MergeStatus::Superseded
    );
}

#[test]
fn generic_nonaccepted_outcomes_do_not_advance_the_contribution_checkpoint() {
    let (_tmp, mut store) = open_temp();
    let session = sample_session(&mut store);

    for (index, status) in [
        MergeStatus::Verified,
        MergeStatus::Rejected,
        MergeStatus::Conflict,
        MergeStatus::Superseded,
    ]
    .into_iter()
    .enumerate()
    {
        let entry = store
            .submit(
                session.id,
                &format!("unaccepted-head-{index}"),
                "integration-base",
            )
            .unwrap();
        store
            .set_merge_status(entry.id, status, Some("candidate-tree"), None)
            .unwrap();
        assert!(
            store
                .record_content_empty_supersession(
                    entry.id,
                    "integration-current",
                    "integration-tree-current",
                    "{}",
                )
                .is_err(),
            "{status:?} must not be accepted through the no-op transition"
        );

        let unchanged = store.session(session.id).unwrap();
        assert_eq!(unchanged.accepted_session_head, None);
        assert_eq!(unchanged.accepted_integration_commit, None);
        assert_eq!(unchanged.accepted_integration_tree, None);
        assert_eq!(unchanged.accepted_queue_entry_id, None);
        assert_eq!(unchanged.accepted_at, None);
    }
}

#[test]
fn events_are_versioned_ordered_and_cursorable() {
    let (_tmp, mut store) = open_temp();
    let session = sample_session(&mut store);
    store
        .append_event(
            "lease.overlap",
            Some(session.id),
            Some("{\"path\":\"src/auth.py\",\"sessions\":[1,2]}"),
        )
        .unwrap();

    let all = store.events_after(0, i64::MAX).unwrap();
    assert!(all.len() >= 2, "mutations emit events automatically");
    assert!(
        all.iter()
            .all(|e| e.schema_version == aethyme_broker::EVENTS_SCHEMA_VERSION)
    );
    assert_eq!(all[0].kind, "session.registered");

    // Cursor semantics for `events --follow`.
    let cursor = all[all.len() - 2].id;
    let tail = store.events_after(cursor, 10).unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].kind, "lease.overlap");
}

#[test]
fn event_stream_filter_prune_and_cursor_survival() {
    let (_tmp, mut store) = open_temp();
    let session = sample_session(&mut store);
    store
        .append_event("lease.overlap", Some(session.id), Some("{}"))
        .unwrap();
    store.submit(session.id, "head1", "base1").unwrap();

    // Prefix filter narrows to a domain.
    let merges = store
        .events_after_filtered(0, i64::MAX, Some("merge."))
        .unwrap();
    assert!(!merges.is_empty());
    assert!(merges.iter().all(|e| e.kind.starts_with("merge.")));
    let exact = store
        .events_after_filtered(0, i64::MAX, Some("lease.overlap"))
        .unwrap();
    assert_eq!(exact.len(), 1);

    // Prune everything (cutoff in the future), then verify ids are NOT
    // reused: the next event continues the sequence, so --since cursors
    // held by consumers remain valid.
    let last_id = store.events_after(0, i64::MAX).unwrap().last().unwrap().id;
    let removed = store.prune_events_before(i64::MAX).unwrap();
    assert!(removed > 0);
    assert!(store.events_after(0, i64::MAX).unwrap().is_empty());
    let new_id = store.append_event("lease.overlap", None, None).unwrap();
    assert!(new_id > last_id, "ids strictly increase across prune");
}
