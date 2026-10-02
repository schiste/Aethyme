//! End-to-end contract for `broker advanced insights`.
//!
//! These tests run against a real broker database with real event rows, because
//! the point of the report is that it is assembled from what actually happened.
//! A funnel that only ever sees a hand-built input proves the arithmetic and
//! nothing about the wiring.
//!
//! Two properties are asserted throughout, and they are the reason this file
//! exists at all:
//!
//! - a figure the broker has not measured is absent, never zero;
//! - nothing in the report identifies a person.
//!
//! One test helper is worth naming. [`event_at`] cannot stamp a time of its own
//! because `append_event` is deliberately now-only — the production path has no
//! reason to backdate an event, and adding a timestamp parameter for tests would
//! make backdating reachable from production. The fixture therefore writes the
//! row through `append_event` and then corrects `ts` with one explicit SQL
//! statement, which is the only place in these tests that reaches past the
//! public API, and it is visible as such.

use std::process::Command;

use aethyme_broker::{
    Broker, BrokerStore, GateStatus, InsightsInput, InsightsQuery, InsightsReport, NewGateResult,
    NewSession, SessionOrigin, SessionStatus, insights,
};

fn git(root: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A repository with a broker and `count` registered sessions.
fn fixture(count: usize) -> (tempfile::TempDir, Broker, Vec<i64>) {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(root.path().join("README.md"), "test\n").unwrap();
    git(root.path(), &["add", "README.md"]);
    git(root.path(), &["commit", "-qm", "initial"]);

    let mut broker = Broker::open(root.path()).unwrap();
    let mut ids = Vec::new();
    for index in 0..count {
        let session = broker
            .store()
            .register_session(&NewSession {
                worktree_path: root
                    .path()
                    .join(format!("wt-{index}"))
                    .display()
                    .to_string(),
                branch: format!("agent/{index}"),
                origin: SessionOrigin::Spawned,
                // Deliberately distinctive: the privacy test asserts none of
                // this reaches the report.
                task: Some(format!("PRIVATE-TASK-TEXT-{index}")),
                diff_base: Some("a".repeat(40)),
                adoption_base: None,
                adopted_head: None,
                repository_contract: None,
                pid: None,
                command: None,
                log_path: None,
                agent_identity: Some("PRIVATE-AGENT-NAME <private@example.test>".into()),
            })
            .unwrap();
        ids.push(session.id);
    }
    (root, broker, ids)
}

/// Append a funnel event, then correct its timestamp.
///
/// `append_event` stamps the current time, which the scenarios below need to be
/// something specific. Two statements rather than one reachable backdating API:
/// the only way to write a past event is this test.
fn event_at(broker: &mut Broker, ts: i64, kind: &str, session_id: i64) {
    let id = broker
        .store()
        .append_event(kind, Some(session_id), Some("{}"))
        .unwrap();
    broker.store().set_event_timestamp_for_test(id, ts);
    broker
        .store()
        .set_session_created_at_for_test(session_id, ts);
}

fn landed_lifecycle(broker: &mut Broker, session_id: i64, base: i64) {
    event_at(broker, base, "session.registered", session_id);
    event_at(broker, base + 60_000, "merge.submitted", session_id);
    event_at(broker, base + 120_000, "merge.verified", session_id);
    event_at(broker, base + 180_000, "merge.promoted", session_id);
}

/// Assemble the report the way the CLI does.
///
/// The same sequence `cli/insights.rs` runs, including
/// [`insights::stage_map`], which folds pull request milestones onto the
/// sessions that own them. Assembling it a second time here, slightly
/// differently, would let a wiring bug pass both the unit tests and these.
fn assemble(store: &BrokerStore) -> InsightsInput {
    let mut events = Vec::new();
    for prefix in insights::STAGE_EVENT_PREFIXES {
        events.extend(
            store
                .events_after_filtered(0, i64::MAX, Some(prefix))
                .unwrap(),
        );
    }
    // Merge events arrive out of order across prefixes; the fold takes the first
    // of each kind by timestamp, so order does not change the answer.
    events.sort_by_key(|event| event.id);
    let folded = insights::fold_events(&events);
    let activity = store.session_activity_totals().unwrap();

    let sessions = store
        .insight_session_rows()
        .unwrap()
        .into_iter()
        .map(|row| {
            let (active_ms, signals) = activity
                .get(&row.session_id)
                .copied()
                .map_or((None, 0), |(ms, signals)| (Some(ms), signals));
            aethyme_broker::SessionInsightRow {
                session_id: row.session_id,
                origin: row.origin,
                created_at: row.created_at,
                closed_at: row.closed_at,
                in_flight: row.in_flight,
                active_ms,
                signals,
            }
        })
        .collect();

    let pull_requests: Vec<aethyme_broker::InsightsPullRequest> = store
        .pull_request_milestones()
        .unwrap()
        .into_iter()
        .map(|(repository, pr_number, opened_at, merged_at)| {
            let session_ids = store.pull_request_sessions(&repository, pr_number).unwrap();
            aethyme_broker::InsightsPullRequest {
                repository,
                pr_number,
                opened_at,
                merged_at,
                session_ids,
            }
        })
        .collect();

    let (succeeded, failed, outcome_unknown) = store.insight_operation_counts(0).unwrap();
    InsightsInput {
        stage_times: insights::stage_map(&folded, &pull_requests),
        failure_times: folded.failures,
        sessions,
        gates: store
            .insight_gate_rows(0)
            .unwrap()
            .into_iter()
            .map(Into::into)
            .collect(),
        gates_cached: 0,
        gates_cached_saved_ms: 0,
        pull_requests,
        overlaps_warned: folded.overlaps_warned,
        conflicts_caught_pre_gate: folded.conflicts_caught_pre_gate,
        out_of_lease_writes: folded.out_of_lease_writes,
        operations_succeeded: succeeded,
        operations_failed: failed,
        operations_outcome_unknown: outcome_unknown,
    }
}

/// Takes `&mut Broker` because `Broker::store` is the only public accessor to
/// the store; `store_ref` is crate-private.
fn report(broker: &mut Broker, query: InsightsQuery) -> InsightsReport {
    insights::report(query, &assemble(broker.store()))
}

#[test]
fn a_landed_lifecycle_is_reported_as_a_full_funnel() {
    let (_root, mut broker, ids) = fixture(1);
    let base = broker.store().newest_event_ts();
    landed_lifecycle(&mut broker, ids[0], base);

    let report = report(&mut broker, InsightsQuery::default());
    assert_eq!(report.funnel.registered, 1);
    assert_eq!(report.funnel.submitted, 1);
    assert_eq!(report.funnel.verified, 1);
    assert_eq!(report.funnel.landed, 1);
    assert_eq!(report.outcomes.landed, 1);
    assert_eq!(report.coverage.sessions, 1);
}

#[test]
fn the_time_between_stages_is_the_difference_of_two_real_events() {
    let (_root, mut broker, ids) = fixture(1);
    let base = broker.store().newest_event_ts();
    landed_lifecycle(&mut broker, ids[0], base);

    let report = report(&mut broker, InsightsQuery::default());
    assert_eq!(report.funnel.to_submit_ms.total_ms, 60_000);
    assert_eq!(report.funnel.to_verify_ms.total_ms, 60_000);
    assert_eq!(report.funnel.to_land_ms.total_ms, 60_000);
}

#[test]
fn work_landed_by_a_human_counts_as_landed() {
    // This repository's own history has 449 `merge.externally_landed` events
    // against 3 pull request observations. Excluding the kind would report the
    // pipeline as slower than it was by exactly the sessions a human rescued.
    let (_root, mut broker, ids) = fixture(1);
    let base = broker.store().newest_event_ts();
    event_at(&mut broker, base, "session.registered", ids[0]);
    event_at(&mut broker, base + 1_000, "merge.submitted", ids[0]);
    event_at(&mut broker, base + 2_000, "merge.externally_landed", ids[0]);

    let report = report(&mut broker, InsightsQuery::default());
    assert_eq!(report.funnel.landed, 1);
    assert_eq!(report.outcomes.landed, 1);
}

#[test]
fn a_rejection_the_session_recovered_from_is_still_a_landing() {
    let (_root, mut broker, ids) = fixture(1);
    let base = broker.store().newest_event_ts();
    landed_lifecycle(&mut broker, ids[0], base);
    event_at(&mut broker, base + 90_000, "merge.rejected", ids[0]);

    let report = report(&mut broker, InsightsQuery::default());
    assert_eq!(report.outcomes.landed, 1);
    assert_eq!(report.outcomes.rejected, 0);
}

#[test]
fn a_rejection_nothing_recovered_from_is_the_session_outcome() {
    let (_root, mut broker, ids) = fixture(1);
    let base = broker.store().newest_event_ts();
    event_at(&mut broker, base, "session.registered", ids[0]);
    event_at(&mut broker, base + 60_000, "merge.submitted", ids[0]);
    event_at(&mut broker, base + 90_000, "merge.rejected", ids[0]);

    let report = report(&mut broker, InsightsQuery::default());
    assert_eq!(report.outcomes.rejected, 1);
    assert_eq!(report.funnel.submitted, 1);
    assert_eq!(report.funnel.landed, 0);
}

#[test]
fn a_session_with_no_activity_history_reports_no_active_time_rather_than_zero() {
    let (_root, mut broker, ids) = fixture(1);
    let base = broker.store().newest_event_ts();
    landed_lifecycle(&mut broker, ids[0], base);

    let report = report(&mut broker, InsightsQuery::default());
    let session = &report.sessions[0];
    assert_eq!(session.active_ms, None);
    // And so no idle time either: wall-clock minus an unknown is unknown, not
    // "all of it was idle".
    assert_eq!(session.idle_ms, None);
    assert_eq!(report.coverage.sessions_without_activity, 1);
    assert_eq!(report.coverage.sessions_with_activity, 0);
}

#[test]
fn recorded_activity_is_summed_and_ends_at_the_close() {
    let (_root, mut broker, ids) = fixture(1);
    let base = broker.store().newest_event_ts();
    landed_lifecycle(&mut broker, ids[0], base);
    let session_id = ids[0];

    // Three signals inside the idle gap: one period of attention.
    broker
        .store()
        .record_session_activity(session_id, base + 1_000)
        .unwrap();
    broker
        .store()
        .record_session_activity(session_id, base + 5_000)
        .unwrap();
    broker
        .store()
        .record_session_activity(session_id, base + 9_000)
        .unwrap();
    broker
        .store()
        .close_session_activity(session_id, base + 60_000)
        .unwrap();

    let report = report(&mut broker, InsightsQuery::default());
    let session = &report.sessions[0];
    assert_eq!(session.active_ms, Some(59_000));
    assert_eq!(session.signals, 3);
    assert_eq!(report.coverage.sessions_with_activity, 1);
    assert_eq!(report.coverage.sessions_without_activity, 0);
}

#[test]
fn a_long_silence_splits_one_worked_period_into_two() {
    // The reason the idle gap exists: without it, an overnight pause and an
    // uninterrupted stretch of work come out the same length.
    let (_root, mut broker, ids) = fixture(1);
    let base = broker.store().newest_event_ts();
    let session_id = ids[0];
    let gap = insights::IDLE_GAP_MS;

    broker
        .store()
        .record_session_activity(session_id, base)
        .unwrap();
    broker
        .store()
        .record_session_activity(session_id, base + gap - 1_000)
        .unwrap();
    // A pause longer than the gap, then a short burst, then the close.
    let resumed = base + gap + 3_600_000;
    broker
        .store()
        .record_session_activity(session_id, resumed)
        .unwrap();
    broker
        .store()
        .close_session_activity(session_id, resumed + 60_000)
        .unwrap();

    let report = report(&mut broker, InsightsQuery::default());
    // First period runs to the last signal before the pause; second runs to the
    // close. The hour of silence contributes nothing.
    assert_eq!(report.sessions[0].active_ms, Some((gap - 1_000) + 60_000));
    assert_eq!(report.sessions[0].signals, 3);
}

#[test]
fn closing_a_session_ends_its_open_period_of_attention() {
    // Otherwise the interval stays open forever and reports an unresolved
    // duration for a session finished a week ago.
    let (_root, mut broker, ids) = fixture(1);
    let session_id = ids[0];
    let base = broker.store().newest_event_ts();
    broker
        .store()
        .record_session_activity(session_id, base)
        .unwrap();
    broker
        .store()
        .set_session_status(session_id, SessionStatus::Closed, None)
        .unwrap();

    let open = broker
        .store()
        .count_session_activity_for_test(session_id, /* open_only = */ true);
    assert_eq!(open, 0);
}

#[test]
fn a_close_long_after_the_last_signal_does_not_count_the_silence() {
    // Cleanup, the sweep and abandonment close sessions hours or days after the
    // agent last acted. A close within the idle gap is the agent finishing its
    // own work; one after it is housekeeping, and crediting the period up to it
    // would make a session cleaned up the next morning look worked overnight.
    let (_root, mut broker, ids) = fixture(1);
    let session_id = ids[0];
    let base = broker.store().newest_event_ts();
    broker
        .store()
        .record_session_activity(session_id, base)
        .unwrap();
    broker
        .store()
        .record_session_activity(session_id, base + 30_000)
        .unwrap();
    broker
        .store()
        .close_session_activity(session_id, base + 30_000 + 86_400_000)
        .unwrap();

    let report = report(&mut broker, InsightsQuery::default());
    assert_eq!(report.sessions[0].active_ms, Some(30_000));
}

#[test]
fn closing_a_session_long_after_its_last_signal_ends_the_period_at_that_signal() {
    // The same rule through a real close path, which is how cleanup ends it.
    let (_root, mut broker, ids) = fixture(1);
    let session_id = ids[0];
    let long_ago = broker.store().newest_event_ts() - 2 * 86_400_000;
    broker
        .store()
        .record_session_activity(session_id, long_ago)
        .unwrap();
    broker
        .store()
        .record_session_activity(session_id, long_ago + 45_000)
        .unwrap();
    broker
        .store()
        .set_session_status(session_id, SessionStatus::Closed, None)
        .unwrap();

    let report = report(&mut broker, InsightsQuery::default());
    let session = report
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .expect("session in the report");
    assert_eq!(session.active_ms, Some(45_000));
}

#[test]
fn an_open_interval_never_counts_the_silence_that_follows_it() {
    // Crediting an open interval with the time since its last signal would make
    // a session idle for a day look like it was worked on for a day.
    let (_root, mut broker, ids) = fixture(1);
    let session_id = ids[0];
    let base = broker.store().newest_event_ts();
    broker
        .store()
        .record_session_activity(session_id, base)
        .unwrap();
    // No close: the interval stays open.

    let report = report(&mut broker, InsightsQuery::default());
    assert_eq!(report.sessions[0].active_ms, Some(0));
}

#[test]
fn a_pull_request_opened_and_merged_reports_its_lifetime() {
    let (_root, mut broker, ids) = fixture(1);
    let base = broker.store().newest_event_ts();
    let session_id = ids[0];
    broker
        .store()
        .record_pull_request_opened("owner/repo", 7, Some(session_id), base)
        .unwrap();
    broker
        .store()
        .record_pull_request_merged("owner/repo", 7, Some(session_id), base + 7_200_000)
        .unwrap();

    let report = report(&mut broker, InsightsQuery::default());
    let pull_request = &report.pull_requests[0];
    assert_eq!(pull_request.pr_number, 7);
    assert_eq!(pull_request.to_merge_ms, Some(7_200_000));
    assert_eq!(pull_request.session_ids, vec![session_id]);
    assert_eq!(report.funnel.pr_opened, 1);
    assert_eq!(report.funnel.pr_merged, 1);
}

#[test]
fn a_second_observer_cannot_make_a_milestone_later() {
    // A watch that starts late still recovers the provider's real `createdAt`.
    // Writing it again must not push an already-recorded opening forward.
    let (_root, mut broker, ids) = fixture(2);
    let base = broker.store().newest_event_ts();
    broker
        .store()
        .record_pull_request_opened("owner/repo", 7, Some(ids[0]), base)
        .unwrap();
    broker
        .store()
        .record_pull_request_opened("owner/repo", 7, Some(ids[1]), base + 86_400_000)
        .unwrap();

    let report = report(&mut broker, InsightsQuery::default());
    assert_eq!(report.pull_requests[0].opened_at, Some(base));
    assert_eq!(
        report.pull_requests[0].session_ids,
        vec![ids[0], ids[1]],
        "both observers stay linked; the link table is many-to-many"
    );
}

#[test]
fn a_pull_request_whose_open_time_was_never_recorded_reports_no_lifetime() {
    let (_root, mut broker, ids) = fixture(1);
    broker
        .store()
        .record_pull_request_merged("owner/repo", 9, Some(ids[0]), 1_000)
        .unwrap();

    let report = report(&mut broker, InsightsQuery::default());
    assert_eq!(report.pull_requests[0].to_merge_ms, None);
    assert_eq!(report.coverage.prs_without_open_time, 1);
    assert_eq!(report.coverage.prs_with_open_time, 0);
}

#[test]
fn gate_waits_are_separated_from_gate_execution() {
    // `wait_duration_ms` has been written on every gate row since v14 and read
    // by nothing. This is its first consumer, and without it "the gates took
    // 90 seconds" cannot be told apart from "the gates waited 89 seconds for a
    // lock and ran for one".
    let (_root, mut broker, ids) = fixture(1);
    broker
        .store()
        .record_gate_result(&NewGateResult {
            gate_name: "test".into(),
            tree_hash: "tree".into(),
            definition_hash: "def".into(),
            status: GateStatus::Pass,
            failure_class: None,
            exit_code: Some(0),
            duration_ms: Some(1_000),
            wait_duration_ms: Some(9_000),
            first_output_ms: Some(250),
            output_bytes: None,
            log_path: None,
            session_id: Some(ids[0]),
        })
        .unwrap();

    let report = report(&mut broker, InsightsQuery::default());
    assert_eq!(report.gates.execute_ms.total_ms, 1_000);
    assert_eq!(report.gates.wait_ms.total_ms, 9_000);
    assert_eq!(report.gates.first_output_ms.total_ms, 250);
    assert_eq!(report.gates_by_name["test"].wait_ms.total_ms, 9_000);
}

#[test]
fn the_json_report_carries_no_person() {
    // `agent_identity` is on the session row and deliberately not selected by
    // `insight_session_rows`. This asserts it at the boundary that matters: the
    // serialized report.
    let (_root, mut broker, ids) = fixture(3);
    let base = broker.store().newest_event_ts();
    landed_lifecycle(&mut broker, ids[0], base);
    broker
        .store()
        .record_session_activity(ids[0], base + 1_000)
        .unwrap();

    let json = serde_json::to_string(&report(&mut broker, InsightsQuery::default())).unwrap();
    for forbidden in [
        "PRIVATE-AGENT-NAME",
        "private@example.test",
        "PRIVATE-TASK-TEXT",
        "wt-0",
        "agent/0",
    ] {
        assert!(
            !json.contains(forbidden),
            "the insights report leaked {forbidden:?}"
        );
    }
}

#[test]
fn the_json_report_names_the_idle_gap_because_it_defines_active_time() {
    let (_root, mut broker, _ids) = fixture(1);
    let json = serde_json::to_value(report(&mut broker, InsightsQuery::default())).unwrap();
    assert_eq!(
        json["idle_gap_ms"].as_i64(),
        Some(insights::IDLE_GAP_MS),
        "active_ms is not interpretable without the threshold that produced it"
    );
}

#[test]
fn the_json_contract_carries_its_schema_version() {
    let (_root, mut broker, _ids) = fixture(1);
    let json = serde_json::to_value(report(&mut broker, InsightsQuery::default())).unwrap();
    assert_eq!(
        json["schema_version"].as_u64(),
        Some(insights::INSIGHTS_SCHEMA_VERSION as u64)
    );
}

#[test]
fn rates_are_absent_without_a_window_and_present_with_one() {
    let (_root, mut broker, ids) = fixture(1);
    let base = broker.store().newest_event_ts();
    landed_lifecycle(&mut broker, ids[0], base);

    let unbounded = report(&mut broker, InsightsQuery::default());
    assert!(unbounded.funnel.per_day.is_none());
    assert!(unbounded.window.is_none());

    let windowed = report(&mut broker, InsightsQuery::last_days(base + 86_400_000, 7));
    let rates = windowed.funnel.per_day.expect("a window was supplied");
    assert!((rates.window_days - 7.0).abs() < 1e-9);
}

#[test]
fn a_percentile_is_refused_below_the_distribution_floor() {
    // Two sessions is below DISTRIBUTION_FLOOR. A p95 over two observations is
    // one of the two observations.
    let (_root, mut broker, ids) = fixture(2);
    let base = broker.store().newest_event_ts();
    landed_lifecycle(&mut broker, ids[0], base);
    landed_lifecycle(&mut broker, ids[1], base + 1_000);

    let report = report(&mut broker, InsightsQuery::default());
    assert_eq!(report.funnel.to_submit_ms.count, 2);
    assert_eq!(report.funnel.to_submit_ms.p50_ms, None);
    assert_eq!(report.funnel.to_submit_ms.p95_ms, None);
    assert!(report.funnel.to_submit_ms.mean_ms.is_some());
}

#[test]
fn a_window_excludes_sessions_outside_it() {
    let (_root, mut broker, ids) = fixture(1);
    let base = broker.store().newest_event_ts();
    landed_lifecycle(&mut broker, ids[0], base);

    // The window sits entirely after the lifecycle.
    let query = InsightsQuery {
        window: Some(aethyme_broker::Window {
            from_ms: base + 10_000_000,
            until_ms: base + 20_000_000,
        }),
        session_limit: 50,
        pull_request_limit: 50,
    };
    let report = report(&mut broker, query);
    assert_eq!(report.coverage.sessions, 0);
    assert_eq!(report.funnel.landed, 0);
}

#[test]
fn the_session_row_list_is_capped_and_says_it_was() {
    let (_root, mut broker, _ids) = fixture(10);
    let report = report(
        &mut broker,
        InsightsQuery {
            window: None,
            session_limit: 3,
            pull_request_limit: 50,
        },
    );
    assert_eq!(report.sessions.len(), 3);
    assert!(report.sessions_truncated);
    assert_eq!(report.sessions_total, 10);
    // The funnel is never truncated by the row limit.
    assert_eq!(report.coverage.sessions, 10);
}

#[test]
fn an_untruncated_list_does_not_claim_to_be_truncated() {
    let (_root, mut broker, _ids) = fixture(2);
    let report = report(&mut broker, InsightsQuery::default());
    assert!(!report.sessions_truncated);
}

#[test]
fn the_report_does_not_modify_the_database_it_reads() {
    // Report-only commands must leave the database byte-identical, or a cron
    // job polling this report would perturb what it measures.
    //
    // Read through a *separate* snapshot connection so the assertion is about
    // the report's own writes rather than about the write connection's WAL.
    let (root, mut broker, ids) = fixture(1);
    let base = broker.store().newest_event_ts();
    landed_lifecycle(&mut broker, ids[0], base);
    let database = root.path().join(".aethyme/broker.db");

    let before = std::fs::read(&database).unwrap();
    {
        let snapshot = BrokerStore::open_snapshot_at(&database).unwrap();
        let assembled = insights::report(InsightsQuery::default(), &assemble(&snapshot));
        assert_eq!(assembled.funnel.landed, 1);
    }

    assert_eq!(
        std::fs::read(&database).unwrap(),
        before,
        "reading the insights report must not modify the database"
    );
}
