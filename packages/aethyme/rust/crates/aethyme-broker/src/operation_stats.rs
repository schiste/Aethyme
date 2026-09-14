//! Bounded coordination-lock measurements.
//!
//! The coordinator deliberately keeps the lock policy unchanged here. This
//! surface answers the question that must precede a finer-grained policy:
//! how long writes hold a lock, how long callers wait, and whether the waits
//! can be shown to be for disjoint known scopes. Samples are read from the
//! additive `coordination_timing` object in operation details, so older rows
//! remain valid but are reported as unmeasured rather than guessed at.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::store::BrokerStore;
use crate::types::{
    CoordinatedOperation, OperationEffect, OperationHistoryQuery, OperationProvider,
};

pub const OPERATION_STATS_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_OPERATION_STATS_LIMIT: u32 = 500;
pub const MAX_OPERATION_STATS_LIMIT: u32 = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct OperationTimingDistribution {
    pub sample_count: usize,
    pub total_ms: i64,
    pub p50_ms: Option<i64>,
    pub p99_ms: Option<i64>,
    pub max_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct OperationQueueDepthStats {
    pub sample_count: usize,
    pub p50: Option<usize>,
    pub p99: Option<usize>,
    pub max: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperationKindStats {
    pub kind: String,
    pub sample_count: usize,
    pub lock_hold_ms: OperationTimingDistribution,
    pub queue_wait_ms: OperationTimingDistribution,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct HooksOutsideLockStats {
    pub sample_count: usize,
    pub lock_hold_ms: OperationTimingDistribution,
    pub queue_wait_ms: OperationTimingDistribution,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UnrelatedContentionStats {
    /// Completed waits for which both sides named disjoint, known scopes.
    /// Unknown or repository-wide scopes are intentionally excluded.
    pub sample_count: usize,
    pub total_queue_wait_ms: i64,
    pub max_queue_wait_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RefDeterminationStats {
    pub pr_merge_operation_count: usize,
    pub measured_count: usize,
    pub succeeded_count: usize,
    pub failed_count: usize,
    pub duration_ms: OperationTimingDistribution,
    pub unmeasured_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperationStats {
    pub schema_version: u32,
    pub repository: Option<String>,
    pub history_limit: u32,
    pub history_truncated: bool,
    pub observed_operations: usize,
    pub measured_operations: usize,
    pub unmeasured_operations: usize,
    pub lock_hold_ms: OperationTimingDistribution,
    pub queue_wait_ms: OperationTimingDistribution,
    pub queue_depth: OperationQueueDepthStats,
    pub by_kind: Vec<OperationKindStats>,
    pub hooks_outside_lock: HooksOutsideLockStats,
    pub unrelated_contention: UnrelatedContentionStats,
    pub pr_merge_ref_determination: RefDeterminationStats,
    pub interpretation: String,
}

#[derive(Debug, Clone)]
struct Timing {
    lock_key: String,
    wait_started_at: Option<i64>,
    acquired_at: Option<i64>,
    released_at: Option<i64>,
    queue_wait_ms: Option<i64>,
    lock_hold_ms: Option<i64>,
    hooks_outside_lock: bool,
    ref_determination_ms: Option<i64>,
    ref_determination_succeeded: Option<bool>,
}

#[derive(Debug, Clone)]
struct Sample {
    operation_id: i64,
    kind: String,
    lock_key: String,
    wait_started_at: i64,
    acquired_at: i64,
    released_at: i64,
    queue_wait_ms: i64,
    lock_hold_ms: i64,
    hooks_outside_lock: bool,
}

pub(crate) fn from_store(
    store: &BrokerStore,
    repository: Option<&str>,
    limit: u32,
) -> Result<OperationStats, crate::BrokerError> {
    if limit == 0 || limit > MAX_OPERATION_STATS_LIMIT {
        return Err(crate::BrokerError::InvalidOperationHistoryLimit {
            limit,
            maximum: MAX_OPERATION_STATS_LIMIT,
        });
    }
    let page = store.operation_history(&OperationHistoryQuery {
        limit,
        repository: repository.map(str::to_owned),
        ..OperationHistoryQuery::default()
    })?;

    let mut samples = Vec::new();
    let mut ref_stats = RefDeterminationStats::default();
    let mut ref_determination_values = Vec::new();
    let mut observed_timing = 0usize;
    for operation in &page.operations {
        let timing = operation_timing(operation);
        if operation_kind(operation) == "gh.pr.merge" {
            ref_stats.pr_merge_operation_count += 1;
            match timing
                .as_ref()
                .and_then(|timing| timing.ref_determination_ms)
            {
                Some(duration_ms) => {
                    ref_stats.measured_count += 1;
                    if timing
                        .as_ref()
                        .and_then(|timing| timing.ref_determination_succeeded)
                        .unwrap_or(false)
                    {
                        ref_stats.succeeded_count += 1;
                    } else {
                        ref_stats.failed_count += 1;
                    }
                    ref_determination_values.push(duration_ms);
                }
                None => ref_stats.unmeasured_count += 1,
            }
        }

        let Some(timing) = timing else {
            continue;
        };
        let (Some(wait_started_at), Some(acquired_at), Some(released_at)) = (
            timing.wait_started_at,
            timing.acquired_at,
            timing.released_at,
        ) else {
            continue;
        };
        let Some(queue_wait_ms) = timing.queue_wait_ms else {
            continue;
        };
        let lock_hold_ms = timing
            .lock_hold_ms
            .or_else(|| released_at.checked_sub(acquired_at))
            .unwrap_or_default();
        if queue_wait_ms < 0 || lock_hold_ms < 0 || released_at < acquired_at {
            continue;
        }
        observed_timing += 1;
        samples.push(Sample {
            operation_id: operation.id,
            kind: operation_kind(operation),
            lock_key: timing.lock_key,
            wait_started_at,
            acquired_at,
            released_at,
            queue_wait_ms,
            lock_hold_ms,
            hooks_outside_lock: timing.hooks_outside_lock,
        });
    }

    let lock_hold_values: Vec<i64> = samples.iter().map(|sample| sample.lock_hold_ms).collect();
    let queue_wait_values: Vec<i64> = samples.iter().map(|sample| sample.queue_wait_ms).collect();
    let queue_depth_values = queue_depths(&samples);

    let mut by_kind = BTreeMap::<String, (Vec<i64>, Vec<i64>)>::new();
    let mut hooks_hold = Vec::new();
    let mut hooks_wait = Vec::new();
    for sample in &samples {
        let entry = by_kind.entry(sample.kind.clone()).or_default();
        entry.0.push(sample.lock_hold_ms);
        entry.1.push(sample.queue_wait_ms);
        if sample.hooks_outside_lock {
            hooks_hold.push(sample.lock_hold_ms);
            hooks_wait.push(sample.queue_wait_ms);
        }
    }
    let by_kind = by_kind
        .into_iter()
        .map(|(kind, (holds, waits))| OperationKindStats {
            kind,
            sample_count: holds.len(),
            lock_hold_ms: distribution(holds),
            queue_wait_ms: distribution(waits),
        })
        .collect();

    let unrelated_contention = unrelated_contention(&samples, &page.operations);
    ref_stats.duration_ms = distribution(ref_determination_values);

    Ok(OperationStats {
        schema_version: OPERATION_STATS_SCHEMA_VERSION,
        repository: repository.map(str::to_owned),
        history_limit: limit,
        history_truncated: page.next_before_id.is_some(),
        observed_operations: page.operations.len(),
        measured_operations: observed_timing,
        unmeasured_operations: page.operations.len().saturating_sub(observed_timing),
        lock_hold_ms: distribution(lock_hold_values),
        queue_wait_ms: distribution(queue_wait_values),
        queue_depth: queue_depth_distribution(queue_depth_values),
        by_kind,
        hooks_outside_lock: HooksOutsideLockStats {
            sample_count: hooks_hold.len(),
            lock_hold_ms: distribution(hooks_hold),
            queue_wait_ms: distribution(hooks_wait),
        },
        unrelated_contention,
        pr_merge_ref_determination: ref_stats,
        interpretation: concat!(
            "Samples are completed operations with broker timing data. ",
            "Unrelated contention counts only waits where both operations ",
            "name disjoint, known scopes; repository-wide or unknown targets ",
            "are excluded. Enable coordination.measure_pr_merge_ref to measure ",
            "the provider read that would determine a pull request's base ref."
        )
        .into(),
    })
}

fn operation_timing(operation: &CoordinatedOperation) -> Option<Timing> {
    let details =
        serde_json::from_str::<serde_json::Value>(operation.details_json.as_deref()?).ok()?;
    let timing = details.get("coordination_timing")?;
    Some(Timing {
        lock_key: timing.get("lock_key")?.as_str()?.to_owned(),
        wait_started_at: timing.get("lock_wait_started_at").and_then(as_i64),
        acquired_at: timing.get("lock_acquired_at").and_then(as_i64),
        released_at: timing.get("lock_released_at").and_then(as_i64),
        queue_wait_ms: timing.get("queue_wait_ms").and_then(as_i64),
        lock_hold_ms: timing.get("lock_hold_ms").and_then(as_i64),
        hooks_outside_lock: timing
            .get("hooks_outside_lock")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        ref_determination_ms: timing.get("ref_determination_ms").and_then(as_i64),
        ref_determination_succeeded: timing
            .get("ref_determination_succeeded")
            .and_then(serde_json::Value::as_bool),
    })
}

fn as_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
}

fn operation_kind(operation: &CoordinatedOperation) -> String {
    let Ok(command) = serde_json::from_str::<Vec<String>>(&operation.command_json) else {
        return format!("{}.unknown", operation.provider.as_str());
    };
    let mut index = 1;
    if operation.provider == OperationProvider::Git {
        while command.get(index).map(String::as_str) == Some("-C") {
            index += 2;
        }
    }
    let command_name = command.get(index).map(String::as_str).unwrap_or("unknown");
    let action = command.get(index + 1).map(String::as_str);
    match operation.provider {
        OperationProvider::Git => format!("git.{command_name}"),
        OperationProvider::Github => match action {
            Some(action) if command_name != "api" => format!("gh.{command_name}.{action}"),
            _ => format!("gh.{command_name}"),
        },
    }
}

fn distribution(mut values: Vec<i64>) -> OperationTimingDistribution {
    values.sort_unstable();
    let sample_count = values.len();
    let total_ms = values.iter().copied().sum();
    OperationTimingDistribution {
        sample_count,
        total_ms,
        p50_ms: percentile(&values, 50),
        p99_ms: percentile(&values, 99),
        max_ms: values.last().copied(),
    }
}

fn percentile(values: &[i64], percentile: usize) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let rank = (values.len() * percentile).div_ceil(100).max(1) - 1;
    values.get(rank).copied()
}

fn queue_depths(samples: &[Sample]) -> Vec<usize> {
    samples
        .iter()
        .filter(|sample| sample.queue_wait_ms > 0)
        .map(|sample| {
            samples
                .iter()
                .filter(|other| {
                    other.lock_key == sample.lock_key
                        && other.queue_wait_ms > 0
                        && other.wait_started_at < sample.wait_started_at
                        && other.acquired_at > sample.wait_started_at
                })
                .count()
                + 1
        })
        .collect()
}

fn queue_depth_distribution(values: Vec<usize>) -> OperationQueueDepthStats {
    if values.is_empty() {
        return OperationQueueDepthStats::default();
    }
    let mut values = values;
    values.sort_unstable();
    let p = |percentile: usize| {
        let rank = (values.len() * percentile).div_ceil(100).max(1) - 1;
        values.get(rank).copied()
    };
    OperationQueueDepthStats {
        sample_count: values.len(),
        p50: p(50),
        p99: p(99),
        max: values.last().copied(),
    }
}

fn unrelated_contention(
    samples: &[Sample],
    operations: &[CoordinatedOperation],
) -> UnrelatedContentionStats {
    let by_id = operations
        .iter()
        .map(|operation| (operation.id, operation))
        .collect::<BTreeMap<_, _>>();
    let mut result = UnrelatedContentionStats::default();
    for sample in samples.iter().filter(|sample| sample.queue_wait_ms > 0) {
        let Some(holder) = samples
            .iter()
            .filter(|candidate| {
                candidate.operation_id != sample.operation_id
                    && candidate.lock_key == sample.lock_key
                    && candidate.acquired_at <= sample.wait_started_at
                    && candidate.released_at >= sample.wait_started_at
            })
            .max_by_key(|candidate| candidate.acquired_at)
        else {
            continue;
        };
        let (Some(waiting), Some(holding)) = (
            by_id.get(&sample.operation_id),
            by_id.get(&holder.operation_id),
        ) else {
            continue;
        };
        if disjoint_known_scopes(waiting, holding) {
            result.sample_count += 1;
            result.total_queue_wait_ms = result
                .total_queue_wait_ms
                .saturating_add(sample.queue_wait_ms);
            result.max_queue_wait_ms = Some(
                result
                    .max_queue_wait_ms
                    .unwrap_or(sample.queue_wait_ms)
                    .max(sample.queue_wait_ms),
            );
        }
    }
    result
}

fn disjoint_known_scopes(left: &CoordinatedOperation, right: &CoordinatedOperation) -> bool {
    if left.effect == OperationEffect::Read || right.effect == OperationEffect::Read {
        return false;
    }
    let left = known_scope(&left.scope);
    let right = known_scope(&right.scope);
    match (left, right) {
        (Some(left), Some(right)) => left != right,
        _ => false,
    }
}

fn known_scope(scope: &str) -> Option<&str> {
    if scope == "repository" || scope.is_empty() {
        return None;
    }
    (scope.starts_with("ref:")
        || scope.starts_with("refs/")
        || scope.starts_with("pull/")
        || scope.starts_with("issue/"))
    .then_some(scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank_and_keep_empty_stats_explicit() {
        let result = distribution(vec![30, 10, 20, 40]);
        assert_eq!(result.sample_count, 4);
        assert_eq!(result.total_ms, 100);
        assert_eq!(result.p50_ms, Some(20));
        assert_eq!(result.p99_ms, Some(40));
        assert_eq!(
            distribution(Vec::new()),
            OperationTimingDistribution::default()
        );
    }

    #[test]
    fn disjoint_scope_detection_is_conservative_for_repository_scope() {
        let operation = |id: i64, scope: &str| CoordinatedOperation {
            id,
            session_id: 1,
            provider: OperationProvider::Git,
            repository: "github.com/o/r".into(),
            scope: scope.into(),
            effect: OperationEffect::Write,
            status: crate::OperationStatus::Succeeded,
            authorization_reason: None,
            command_json: "[\"git\",\"push\"]".into(),
            pid: 1,
            exit_code: Some(0),
            details_json: None,
            created_at: 1,
            updated_at: 2,
            finished_at: Some(2),
            host_operation_id: None,
            identity_provenance: crate::OperationIdentityProvenance::VerifiedCanonical,
        };
        assert!(disjoint_known_scopes(
            &operation(1, "ref:one"),
            &operation(2, "ref:two")
        ));
        assert!(!disjoint_known_scopes(
            &operation(1, "repository"),
            &operation(2, "ref:two")
        ));
    }

    #[test]
    fn stats_group_completed_timings_and_count_only_known_unrelated_waits() {
        let temporary = tempfile::tempdir().unwrap();
        let database = temporary.path().join("broker.db");
        drop(BrokerStore::open(&database).unwrap());
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute(
                "INSERT INTO sessions (
                     id, worktree_path, branch, origin, status,
                     created_at, updated_at, last_activity_at
                 ) VALUES (1, '/repo', 'agent/one', 'adopted', 'active', 1, 1, 1)",
                [],
            )
            .unwrap();
        drop(connection);
        let mut store = BrokerStore::open(&database).unwrap();

        let record =
            |store: &mut BrokerStore, command: &str, scope: &str, timing: serde_json::Value| {
                let operation = store
                    .create_coordinated_operation(&crate::NewCoordinatedOperation {
                        session_id: 1,
                        provider: if command.starts_with("[\"gh\"") {
                            OperationProvider::Github
                        } else {
                            OperationProvider::Git
                        },
                        repository: "github.com/o/r".into(),
                        scope: scope.into(),
                        effect: OperationEffect::Write,
                        authorization_reason: Some("test".into()),
                        command_json: command.into(),
                        pid: 1,
                        host_operation_id: None,
                        identity_provenance: crate::OperationIdentityProvenance::VerifiedCanonical,
                    })
                    .unwrap();
                let details = serde_json::json!({ "coordination_timing": timing }).to_string();
                store
                    .transition_coordinated_operation(
                        operation.id,
                        crate::OperationStatus::Succeeded,
                        Some(0),
                        Some(&details),
                    )
                    .unwrap();
            };

        let timing = |lock_key, wait_started, acquired, released, queue_wait, hold, hooks| {
            serde_json::json!({
                "lock_key": lock_key,
                "lock_wait_started_at": wait_started,
                "lock_acquired_at": acquired,
                "lock_released_at": released,
                "queue_wait_ms": queue_wait,
                "lock_hold_ms": hold,
                "hooks_outside_lock": hooks,
            })
        };
        record(
            &mut store,
            "[\"git\",\"push\"]",
            "ref:one",
            timing("github.com/o/r", 100, 100, 500, 0, 400, false),
        );
        record(
            &mut store,
            "[\"git\",\"push\"]",
            "ref:two",
            timing("github.com/o/r", 200, 600, 700, 400, 100, true),
        );
        let mut merge_timing = timing("github.com/o/r", 800, 800, 900, 0, 100, false);
        merge_timing["ref_determination_ms"] = serde_json::json!(12);
        merge_timing["ref_determination_succeeded"] = serde_json::json!(true);
        record(
            &mut store,
            "[\"gh\",\"pr\",\"merge\",\"7\"]",
            "repository",
            merge_timing,
        );

        let report = from_store(&store, None, 50).unwrap();
        assert_eq!(report.observed_operations, 3);
        assert_eq!(report.measured_operations, 3);
        assert_eq!(report.lock_hold_ms.p50_ms, Some(100));
        assert_eq!(report.lock_hold_ms.p99_ms, Some(400));
        assert_eq!(report.queue_wait_ms.p99_ms, Some(400));
        assert_eq!(report.queue_depth.sample_count, 1);
        assert_eq!(report.queue_depth.max, Some(1));
        assert_eq!(report.unrelated_contention.sample_count, 1);
        assert_eq!(report.unrelated_contention.total_queue_wait_ms, 400);
        assert_eq!(report.hooks_outside_lock.sample_count, 1);
        assert_eq!(report.pr_merge_ref_determination.measured_count, 1);
        assert_eq!(
            report.pr_merge_ref_determination.duration_ms.p50_ms,
            Some(12)
        );
        assert_eq!(report.by_kind.len(), 2);
    }
}
