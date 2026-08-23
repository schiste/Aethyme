//! Event-contract freeze tests (schema_version 1): golden expectations
//! for the kind catalog, the envelope row shape, and per-kind payload
//! field names. `docs/events-contract.md` declares v1 FROZEN; these
//! tests are the enforcement — an unversioned rename/removal fails here.
//!
//! On failure, do NOT edit the goldens to match the code. Either revert
//! to an additive change, or follow the bump procedure in
//! `docs/events-contract.md` (bump `EVENTS_SCHEMA_VERSION`, update the
//! goldens, the doc, and the module in the same commit).

use std::path::Path;
use std::process::Command;

use aethyme_broker::{
    Broker, EVENTS_SCHEMA_VERSION, Event, FinishDelivery, FinishGateCacheSource, FinishGateRun,
    FinishLease, FinishLeaseState, FinishPendingWork, FinishReport, FinishStatus, GateStatus,
    LeaseKind, MergeStatus, OperationEffect, OperationProvider, OperationStatus, Overlap,
    SessionStatus, events,
};

/// The complete v1 kind catalog. Additions append here (additive change);
/// any rename or removal is a break that requires a schema_version bump.
const V1_KINDS: &[&str] = &[
    "gate.cached",
    "gate.cancelled",
    "gate.error",
    "gate.fail",
    "gate.pass",
    "lease.claimed",
    "lease.overlap",
    "lease.released",
    "merge.conflict",
    "merge.externally_landed",
    "merge.integration_branch_created",
    "merge.integration_refreshed",
    "merge.promoted",
    "merge.rejected",
    "merge.simulating",
    "merge.submitted",
    "merge.superseded",
    "merge.verified",
    "operation.failed",
    "operation.outcome_unknown",
    "operation.prepared",
    "operation.reconciled_failed",
    "operation.reconciled_succeeded",
    "operation.running",
    "operation.succeeded",
    "session.active",
    "session.cleaned",
    "session.exited",
    "session.finished",
    "session.idle",
    "session.registered",
    "session.reused",
    "session.stale",
];

/// Sorted top-level field names of a JSON object payload.
fn keys(json: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    let mut keys: Vec<String> = value
        .as_object()
        .unwrap_or_else(|| panic!("payload is not a JSON object: {json}"))
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn assert_keys(json: &str, expected: &[&str], context: &str) {
    assert_eq!(keys(json), expected, "field names changed for {context}");
}

#[test]
fn schema_version_is_one_and_envelope_shape_is_frozen() {
    assert_eq!(
        EVENTS_SCHEMA_VERSION, 1,
        "EVENTS_SCHEMA_VERSION changed — follow the bump procedure in \
         docs/events-contract.md and update this crate's contract goldens"
    );
    let row = Event {
        id: 1,
        schema_version: EVENTS_SCHEMA_VERSION,
        ts: 0,
        kind: "session.registered".into(),
        session_id: None,
        payload_json: None,
    };
    assert_keys(
        &serde_json::to_string(&row).unwrap(),
        &[
            "id",
            "kind",
            "payload_json",
            "schema_version",
            "session_id",
            "ts",
        ],
        "the event envelope row (`events --json` line format)",
    );
}

#[test]
fn v1_kind_catalog_is_frozen() {
    // Reassemble every kind the code can emit: constants from
    // `events.rs` plus the status-enum-derived `<domain>.<status>` kinds
    // (the store formats these from `as_str`, so renaming an enum text
    // lands here).
    let mut actual: Vec<String> = vec![
        events::SESSION_REGISTERED.into(),
        events::SESSION_REUSED.into(),
        events::SESSION_FINISHED.into(),
        events::LEASE_CLAIMED.into(),
        events::LEASE_RELEASED.into(),
        events::LEASE_OVERLAP.into(),
        events::GATE_CACHED.into(),
        events::MERGE_INTEGRATION_BRANCH_CREATED.into(),
        events::MERGE_INTEGRATION_REFRESHED.into(),
    ];
    for status in [
        SessionStatus::Active,
        SessionStatus::Idle,
        SessionStatus::Stale,
        SessionStatus::Exited,
        SessionStatus::Cleaned,
    ] {
        actual.push(format!("session.{}", status.as_str()));
    }
    for status in [
        GateStatus::Pass,
        GateStatus::Fail,
        GateStatus::Cancelled,
        GateStatus::Error,
    ] {
        actual.push(format!("gate.{}", status.as_str()));
    }
    for status in [
        MergeStatus::Submitted,
        MergeStatus::Simulating,
        MergeStatus::Conflict,
        MergeStatus::Verified,
        MergeStatus::Promoted,
        MergeStatus::ExternallyLanded,
        MergeStatus::Rejected,
        MergeStatus::Superseded,
    ] {
        actual.push(format!("merge.{}", status.as_str()));
    }
    for status in [
        OperationStatus::Prepared,
        OperationStatus::Running,
        OperationStatus::Succeeded,
        OperationStatus::Failed,
        OperationStatus::OutcomeUnknown,
        OperationStatus::ReconciledSucceeded,
        OperationStatus::ReconciledFailed,
    ] {
        actual.push(format!("operation.{}", status.as_str()));
    }
    actual.sort();
    assert_eq!(
        actual, V1_KINDS,
        "the v1 kind catalog changed — additions are fine (append to the \
         golden and docs/events-contract.md); renames/removals require a \
         schema_version bump"
    );
}

#[test]
fn v1_constructor_payload_field_names_are_frozen() {
    assert_keys(
        &events::session_registered_payload("adopted", "b", "w"),
        &["branch", "origin", "worktree_path"],
        "session.registered",
    );
    assert_keys(
        &events::session_reused_payload(Some("t"), Some("d")),
        &["diff_base", "task"],
        "session.reused",
    );
    assert_keys(
        &events::session_exit_payload(0),
        &["exit_code"],
        "session.exited",
    );
    let finish = FinishReport {
        session_id: 7,
        worktree_path: "/secret/worktree".into(),
        status: FinishStatus::Closed,
        closed: true,
        dirty_paths: Vec::new(),
        unsubmitted_commits: 0,
        latest_queue_entry_id: Some(9),
        latest_queue_status: Some(MergeStatus::Promoted),
        delivery: FinishDelivery {
            submitted: true,
            promoted: true,
            published: false,
            promotion_commit: Some("commit".into()),
        },
        pending_work: FinishPendingWork::default(),
        leases_held: vec![FinishLease {
            path: "src/".into(),
            kind: LeaseKind::Explicit,
            state: FinishLeaseState::Active,
            expires_at: None,
            released_at: None,
        }],
        last_gate: Some(FinishGateRun {
            gate: "test".into(),
            status: GateStatus::Pass,
            tree_hash: "tree".into(),
            recorded_at: 1,
            cache_source: FinishGateCacheSource::Executed,
        }),
        cleanup_safe: false,
        recommended_next_action: Some("next".into()),
        summary: "secret summary".into(),
        warnings: vec!["secret warning".into()],
        next_commands: vec!["secret command".into()],
    };
    let finished_payload = events::session_finished_payload(&finish);
    assert_keys(
        &finished_payload,
        &[
            "cleanup_safe",
            "delivery",
            "last_gate",
            "latest_queue_entry_id",
            "latest_queue_status",
            "leases_held",
            "pending_work",
            "recommended_next_action",
            "session_id",
            "status",
        ],
        "session.finished",
    );
    let finished: serde_json::Value = serde_json::from_str(&finished_payload).unwrap();
    assert_keys(
        &finished["delivery"].to_string(),
        &["promoted", "promotion_commit", "published", "submitted"],
        "session.finished delivery",
    );
    assert_keys(
        &finished["pending_work"].to_string(),
        &[
            "dirty_path_count",
            "present",
            "unsubmitted_commits",
            "worktree_missing",
        ],
        "session.finished pending_work",
    );
    assert_keys(
        &finished["leases_held"][0].to_string(),
        &["expires_at", "kind", "path", "released_at", "state"],
        "session.finished lease",
    );
    assert_keys(
        &finished["last_gate"].to_string(),
        &["cache_source", "gate", "recorded_at", "status", "tree_hash"],
        "session.finished last_gate",
    );
    assert!(!finished_payload.contains("/secret/worktree"));
    assert!(!finished_payload.contains("secret summary"));
    assert!(!finished_payload.contains("secret warning"));
    assert!(!finished_payload.contains("secret command"));
    assert_keys(
        &events::lease_path_payload("p"),
        &["path"],
        "lease.claimed / lease.released",
    );
    assert_keys(
        &events::gate_result_payload("g", "t", None),
        &["failure_class", "gate", "tree"],
        "gate.pass / gate.fail / gate.cancelled / gate.error",
    );
    assert_keys(
        &events::gate_cached_payload("g", "t", 1, GateStatus::Pass, None),
        &["cached_status", "failure_class", "gate", "saved_ms", "tree"],
        "gate.cached",
    );
    assert_keys(
        &events::merge_submitted_payload("h"),
        &["head"],
        "merge.submitted",
    );
    assert_keys(
        &events::operation_payload(
            7,
            OperationProvider::Github,
            "owner/repo",
            "pull-request:12",
            OperationEffect::Write,
            OperationStatus::Running,
            None,
        ),
        &[
            "effect",
            "exit_code",
            "operation_id",
            "provider",
            "repository",
            "scope",
            "status",
        ],
        "operation.<status>",
    );
    assert_keys(
        &events::integration_branch_created_payload("b", "c"),
        &["at", "branch"],
        "merge.integration_branch_created",
    );
    assert_keys(
        &events::integration_refreshed_payload("b", "f", "t"),
        &["branch", "from", "to"],
        "merge.integration_refreshed",
    );
    assert_keys(
        &events::merge_promoted_payload("b", "c"),
        &["branch", "commit"],
        "merge.promoted",
    );
    assert_keys(
        &events::merge_externally_landed_payload(
            "b",
            "c",
            "already_landed",
            "origin/main",
            Some("u"),
            None,
        ),
        &[
            "branch",
            "classification",
            "commit",
            "externally_landed",
            "operator_resolution",
            "upstream_landing",
            "upstream_ref",
        ],
        "merge.externally_landed",
    );
    // lease.overlap serializes the Overlap struct directly.
    let overlap = Overlap {
        session_a: 1,
        session_b: 2,
        path: "p".into(),
    };
    assert_keys(
        &serde_json::to_string(&overlap).unwrap(),
        &["path", "session_a", "session_b"],
        "lease.overlap",
    );
}

// ── merge lifecycle payloads (built in merge.rs, not events.rs) ────────
// merge.conflict / merge.verified / merge.rejected payloads are the
// queue entry's details_json — exercised end-to-end so the goldens lock
// what real consumers see on the wire.

fn sh(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.py"), "a = 1\n").unwrap();
    std::fs::write(root.join("src/b.py"), "b = 1\n").unwrap();
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"ok\"\ncommand = \"true\"\ntriggers = [\"**/*.py\"]\n",
    )
    .unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn agent_worktree(root: &Path, name: &str) -> std::path::PathBuf {
    let path = root.join(".aethyme/worktrees").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    sh(
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

fn commit_edit(worktree: &Path, file: &str, content: &str) {
    std::fs::write(worktree.join(file), content).unwrap();
    sh(worktree, &["add", "-A"]);
    sh(worktree, &["commit", "-qm", "edit"]);
}

#[test]
fn merge_lifecycle_payload_field_names_are_frozen_on_the_wire() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    // Verified + promoted: session A lands a clean change through a
    // passing gate.
    let wt_a = agent_worktree(tmp.path(), "a");
    let a = broker.adopt(&wt_a, Some("a")).unwrap();
    commit_edit(&wt_a, "src/a.py", "a = 2\n");
    assert!(broker.submit(a.id).unwrap().promoted);

    // Conflict: session B edits the same line A already promoted.
    let wt_b = agent_worktree(tmp.path(), "b");
    let b = broker.adopt(&wt_b, Some("b")).unwrap();
    commit_edit(&wt_b, "src/a.py", "a = 3\n");
    let out = broker.submit(b.id).unwrap();
    assert_eq!(out.entry.status, MergeStatus::Conflict);

    // Rejected: session C's clean merge fails the gate policy carried by
    // its submitted tree.
    let wt_c = agent_worktree(tmp.path(), "c");
    let c = broker.adopt(&wt_c, Some("c")).unwrap();
    std::fs::write(
        wt_c.join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"no\"\ncommand = \"exit 7\"\ntriggers = [\"**/*.py\"]\n",
    )
    .unwrap();
    commit_edit(&wt_c, "src/b.py", "b = 2\n");
    let out = broker.submit(c.id).unwrap();
    assert_eq!(out.entry.status, MergeStatus::Rejected);

    let events = broker.store().events_after(0, i64::MAX).unwrap();

    // Every kind this lifecycle emitted is in the frozen catalog.
    for event in &events {
        assert!(
            V1_KINDS.contains(&event.kind.as_str()),
            "emitted kind {:?} is not in the v1 catalog — append it to \
             V1_KINDS and docs/events-contract.md",
            event.kind
        );
        assert_eq!(event.schema_version, EVENTS_SCHEMA_VERSION);
    }
    let payload_of = |kind: &str| -> &str {
        events
            .iter()
            .find(|e| e.kind == kind)
            .unwrap_or_else(|| panic!("lifecycle did not emit {kind}"))
            .payload_json
            .as_deref()
            .unwrap_or_else(|| panic!("{kind} has no payload"))
    };

    assert_keys(
        payload_of("merge.conflict"),
        &["base", "blocking_sessions", "conflicts"],
        "merge.conflict",
    );
    for kind in ["merge.verified", "merge.rejected"] {
        let payload = payload_of(kind);
        assert_keys(payload, &["base", "gates", "merge_commit"], kind);
        let value: serde_json::Value = serde_json::from_str(payload).unwrap();
        let gates = value["gates"].as_array().unwrap();
        assert!(!gates.is_empty(), "{kind} ran at least one gate");
        for gate in gates {
            assert_keys(
                &gate.to_string(),
                &["cached", "failure_class", "gate", "status", "tree_hash"],
                &format!("{kind} gates[] element"),
            );
        }
    }
}
