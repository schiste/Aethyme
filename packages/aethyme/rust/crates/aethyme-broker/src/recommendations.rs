//! Bounded, privacy-preserving repository-quality recommendations.
//!
//! Producers consume typed broker history and only project allowlisted facts.
//! They never inspect task text, command output, diffs, file contents,
//! environment values, or absolute paths.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    AdvisoryAudience, AdvisoryEvidence, AdvisoryProducer, AdvisorySeverity, Event,
    GateFailureClass, GateResult, GateStatus, Lease, MergeQueueEntry, MergeStatus,
};

pub const RECOMMENDATION_SCHEMA_VERSION: u32 = 1;
pub const RECOMMENDATION_HISTORY_LIMIT: usize = 200;
pub const RECOMMENDATION_MIN_SAMPLES: usize = 3;
pub const RECOMMENDATION_GATE_HISTORY_LIMIT: usize = 500;
pub const RECOMMENDATION_DURATION_MIN_SAMPLES: usize = 10;
pub const RECOMMENDATION_SLOW_MEDIAN_MS: i64 = 30_000;
pub const RECOMMENDATION_SLOW_P95_MS: i64 = 120_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationKind {
    RepeatedMergeConflict,
    SessionPassMergedFail,
    RepeatedInfrastructureFailure,
    SlowGate,
    RepeatedCacheBypass,
    OutOfLeaseWrites,
    UntrackedRuntimeArtifacts,
    ResourceContention,
    LeaseExpiration,
}

impl RecommendationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RepeatedMergeConflict => "repeated_merge_conflict",
            Self::SessionPassMergedFail => "session_pass_merged_fail",
            Self::RepeatedInfrastructureFailure => "repeated_infrastructure_failure",
            Self::SlowGate => "slow_gate",
            Self::RepeatedCacheBypass => "repeated_cache_bypass",
            Self::OutOfLeaseWrites => "out_of_lease_writes",
            Self::UntrackedRuntimeArtifacts => "untracked_runtime_artifacts",
            Self::ResourceContention => "resource_contention",
            Self::LeaseExpiration => "lease_expiration",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendationConfidence {
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintainerRecommendation {
    pub identity: String,
    pub audience: AdvisoryAudience,
    pub producer: AdvisoryProducer,
    pub kind: RecommendationKind,
    pub severity: AdvisorySeverity,
    pub confidence: RecommendationConfidence,
    pub sample_count: usize,
    pub window_size: usize,
    pub paths: Vec<String>,
    pub evidence: Vec<AdvisoryEvidence>,
    pub remediation: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecommendationInspection {
    pub state: &'static str,
    pub reason: Option<String>,
    pub recommendations: Vec<MaintainerRecommendation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecommendationSnapshot {
    pub recommendations: Vec<MaintainerRecommendation>,
    pub saturated_kinds: Vec<RecommendationKind>,
}

pub(crate) fn inspect_history_recommendations(main_root: &Path) -> RecommendationInspection {
    let database = main_root.join(crate::BROKER_DB_RELPATH);
    match std::fs::symlink_metadata(&database) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => RecommendationInspection {
            state: "missing",
            reason: Some("broker history does not exist".into()),
            recommendations: Vec::new(),
        },
        Err(error) => RecommendationInspection {
            state: "inaccessible",
            reason: Some(error.to_string()),
            recommendations: Vec::new(),
        },
        Ok(metadata) if !metadata.is_file() => RecommendationInspection {
            state: "inaccessible",
            reason: Some("broker history path is not a regular file".into()),
            recommendations: Vec::new(),
        },
        Ok(_) => match crate::BrokerStore::open_current_read_only_in_repo(main_root)
            .and_then(|store| recommendations_from_store(&store))
        {
            Ok(recommendations) => RecommendationInspection {
                state: "ready",
                reason: None,
                recommendations,
            },
            Err(error) => RecommendationInspection {
                state: "inaccessible",
                reason: Some(error.to_string()),
                recommendations: Vec::new(),
            },
        },
    }
}

pub(crate) fn recommendations_from_store(
    store: &crate::BrokerStore,
) -> Result<Vec<MaintainerRecommendation>, crate::BrokerError> {
    let snapshot = recommendation_snapshot_from_store(store)?;
    let controls = store.advisories(true)?;
    Ok(snapshot
        .recommendations
        .into_iter()
        .filter(|recommendation| {
            let Some(control) = controls
                .iter()
                .find(|advisory| advisory.identity == recommendation.identity)
            else {
                return true;
            };
            if control.audience != AdvisoryAudience::Maintainer
                || control.producer != recommendation.producer
            {
                return true;
            }
            match control.resolution_state {
                crate::AdvisoryResolutionState::Outstanding => true,
                crate::AdvisoryResolutionState::Acknowledged
                | crate::AdvisoryResolutionState::Suppressed => {
                    control.evidence != recommendation.to_new_advisory().evidence
                }
                crate::AdvisoryResolutionState::Resolved => true,
            }
        })
        .collect())
}

pub(crate) fn recommendation_snapshot_from_store(
    store: &crate::BrokerStore,
) -> Result<RecommendationSnapshot, crate::BrokerError> {
    let merges = store.recent_merge_history(RECOMMENDATION_HISTORY_LIMIT)?;
    let gates = store.recent_gate_results_for_recommendations(RECOMMENDATION_GATE_HISTORY_LIMIT)?;
    let events = store.recent_events(RECOMMENDATION_GATE_HISTORY_LIMIT as i64, None)?;
    let leases = store.recent_leases_for_recommendations(RECOMMENDATION_HISTORY_LIMIT)?;
    let observed_at = merges
        .iter()
        .map(|entry| entry.updated_at)
        .chain(gates.iter().map(|result| result.created_at))
        .chain(events.iter().map(|event| event.ts))
        .chain(
            leases
                .iter()
                .flat_map(|lease| [Some(lease.created_at), lease.released_at])
                .flatten(),
        )
        .max()
        .unwrap_or_default();
    let mut recommendations = derive_conflict_recommendations(&merges);
    recommendations.extend(derive_gate_recommendations(&gates, &merges, &events));
    recommendations.extend(derive_isolation_resource_recommendations(
        &events,
        &gates,
        &leases,
        observed_at,
    ));
    recommendations.sort_by(|left, right| left.identity.cmp(&right.identity));
    let mut saturated_kinds = Vec::new();
    if merges.len() == RECOMMENDATION_HISTORY_LIMIT {
        saturated_kinds.push(RecommendationKind::RepeatedMergeConflict);
    }
    if gates.len() == RECOMMENDATION_GATE_HISTORY_LIMIT {
        saturated_kinds.extend([
            RecommendationKind::SessionPassMergedFail,
            RecommendationKind::RepeatedInfrastructureFailure,
            RecommendationKind::SlowGate,
            RecommendationKind::ResourceContention,
        ]);
    }
    if events
        .iter()
        .filter(|event| event.kind == crate::events::GATE_CACHE_BYPASSED)
        .count()
        == RECOMMENDATION_GATE_HISTORY_LIMIT
    {
        saturated_kinds.push(RecommendationKind::RepeatedCacheBypass);
    }
    if events
        .iter()
        .filter(|event| event.kind == crate::events::GUARD_OUT_OF_LEASE_WRITE)
        .count()
        == RECOMMENDATION_GATE_HISTORY_LIMIT
    {
        saturated_kinds.push(RecommendationKind::OutOfLeaseWrites);
    }
    if events
        .iter()
        .filter(|event| event.kind == crate::events::GUARD_UNTRACKED_ARTIFACT)
        .count()
        == RECOMMENDATION_GATE_HISTORY_LIMIT
    {
        saturated_kinds.push(RecommendationKind::UntrackedRuntimeArtifacts);
    }
    if leases.len() == RECOMMENDATION_HISTORY_LIMIT {
        saturated_kinds.push(RecommendationKind::LeaseExpiration);
    }
    Ok(RecommendationSnapshot {
        recommendations,
        saturated_kinds,
    })
}

impl MaintainerRecommendation {
    pub fn to_new_advisory(&self) -> crate::NewAdvisory {
        let mut evidence = vec![
            evidence("recommendation_kind", self.kind.as_str()),
            evidence(
                "confidence",
                serde_json::to_value(self.confidence)
                    .expect("recommendation confidence serializes")
                    .as_str()
                    .expect("recommendation confidence is a string"),
            ),
            evidence(
                "recommendation_sample_count",
                &self.sample_count.to_string(),
            ),
            evidence("remediation", &self.remediation),
        ];
        evidence.extend(self.evidence.clone());
        crate::NewAdvisory {
            identity: self.identity.clone(),
            audience: self.audience,
            producer: self.producer,
            session_id: None,
            severity: self.severity,
            queue_entry_id: None,
            integration_sha: None,
            paths: self.paths.clone(),
            evidence,
        }
    }
}

/// Derive repeated-conflict recommendations from a newest-first bounded
/// merge window. Only the allowlisted `conflicts` string array is read from
/// details JSON; all other fields are ignored.
pub fn derive_conflict_recommendations(
    entries: &[MergeQueueEntry],
) -> Vec<MaintainerRecommendation> {
    let entries = &entries[..entries.len().min(RECOMMENDATION_HISTORY_LIMIT)];
    let mut paths = BTreeMap::<String, Vec<i64>>::new();
    for entry in entries {
        if entry.status != MergeStatus::Conflict {
            continue;
        }
        for path in conflict_paths(entry.details_json.as_deref()) {
            paths.entry(path).or_default().push(entry.id);
        }
    }

    paths
        .into_iter()
        .filter(|(_, ids)| ids.len() >= RECOMMENDATION_MIN_SAMPLES)
        .map(|(path, mut ids)| {
            ids.sort_unstable();
            let sample_count = ids.len();
            let identity = stable_identity(RecommendationKind::RepeatedMergeConflict, &path);
            MaintainerRecommendation {
                identity,
                audience: AdvisoryAudience::Maintainer,
                producer: AdvisoryProducer::ConflictHistory,
                kind: RecommendationKind::RepeatedMergeConflict,
                severity: AdvisorySeverity::Warning,
                confidence: RecommendationConfidence::High,
                sample_count,
                window_size: entries.len(),
                paths: vec![path],
                evidence: vec![
                    AdvisoryEvidence {
                        kind: "sample_count".into(),
                        summary: format!(
                            "{sample_count} conflicting submissions in the bounded history window"
                        ),
                    },
                    AdvisoryEvidence {
                        kind: "queue_entries".into(),
                        summary: ids
                            .iter()
                            .take(8)
                            .map(i64::to_string)
                            .collect::<Vec<_>>()
                            .join(","),
                    },
                ],
                remediation: "review ownership boundaries and split or sequence work on this path"
                    .into(),
            }
        })
        .collect()
}

pub fn derive_gate_recommendations(
    results: &[GateResult],
    merges: &[MergeQueueEntry],
    events: &[Event],
) -> Vec<MaintainerRecommendation> {
    let results = &results[..results.len().min(RECOMMENDATION_GATE_HISTORY_LIMIT)];
    let merge_trees = merges
        .iter()
        .filter_map(|entry| entry.merged_tree.as_deref())
        .collect::<BTreeSet<_>>();
    let mut by_gate = BTreeMap::<String, Vec<&GateResult>>::new();
    for result in results {
        by_gate
            .entry(result.gate_name.clone())
            .or_default()
            .push(result);
    }

    let mut recommendations = Vec::new();
    for (gate, samples) in by_gate {
        let safe_gate = safe_label(&gate);
        let merged_failures = samples
            .iter()
            .filter(|sample| {
                merge_trees.contains(sample.tree_hash.as_str())
                    && !matches!(sample.status, GateStatus::Pass | GateStatus::Cancelled)
            })
            .count();
        let session_passes = samples
            .iter()
            .filter(|sample| {
                !merge_trees.contains(sample.tree_hash.as_str())
                    && sample.status == GateStatus::Pass
            })
            .count();
        if merged_failures >= RECOMMENDATION_MIN_SAMPLES
            && session_passes >= RECOMMENDATION_MIN_SAMPLES
        {
            recommendations.push(gate_recommendation(
                RecommendationKind::SessionPassMergedFail,
                AdvisoryProducer::GateReliabilityHistory,
                &gate,
                merged_failures,
                results.len(),
                RecommendationConfidence::High,
                vec![
                    evidence("gate", &safe_gate),
                    evidence(
                        "session_passes",
                        &format!("{session_passes} passing session-tree runs"),
                    ),
                    evidence(
                        "merged_failures",
                        &format!("{merged_failures} failing merged-tree runs"),
                    ),
                ],
                "make the gate worktree-isolated and reproduce it against the merged tree",
            ));
        }

        let infrastructure_failures = samples
            .iter()
            .filter(|sample| {
                matches!(
                    sample.failure_class,
                    Some(
                        GateFailureClass::Environment
                            | GateFailureClass::ResourceContention
                            | GateFailureClass::Timeout
                    )
                )
            })
            .count();
        if infrastructure_failures >= RECOMMENDATION_MIN_SAMPLES {
            recommendations.push(gate_recommendation(
                RecommendationKind::RepeatedInfrastructureFailure,
                AdvisoryProducer::GateReliabilityHistory,
                &gate,
                infrastructure_failures,
                results.len(),
                RecommendationConfidence::High,
                vec![
                    evidence("gate", &safe_gate),
                    evidence(
                        "infrastructure_failures",
                        &format!("{infrastructure_failures} typed infrastructure failures"),
                    ),
                ],
                "separate infrastructure setup from test failures and review resource declarations",
            ));
        }

        let mut durations = samples
            .iter()
            .filter_map(|sample| sample.duration_ms)
            .filter(|duration| *duration >= 0)
            .collect::<Vec<_>>();
        if durations.len() >= RECOMMENDATION_DURATION_MIN_SAMPLES {
            durations.sort_unstable();
            let median = durations[(durations.len() - 1) / 2];
            let p95_index = ((durations.len() * 95).div_ceil(100)).saturating_sub(1);
            let p95 = durations[p95_index];
            if median >= RECOMMENDATION_SLOW_MEDIAN_MS || p95 >= RECOMMENDATION_SLOW_P95_MS {
                recommendations.push(gate_recommendation(
                    RecommendationKind::SlowGate,
                    AdvisoryProducer::GateReliabilityHistory,
                    &gate,
                    durations.len(),
                    results.len(),
                    RecommendationConfidence::High,
                    vec![
                        evidence("gate", &safe_gate),
                        evidence("duration_samples", &durations.len().to_string()),
                        evidence("median_ms", &median.to_string()),
                        evidence("p95_ms", &p95.to_string()),
                    ],
                    "split or narrow the gate while preserving repository validation semantics",
                ));
            }
        }
    }

    let mut bypasses = BTreeMap::<String, usize>::new();
    for event in events
        .iter()
        .take(RECOMMENDATION_GATE_HISTORY_LIMIT)
        .filter(|event| event.kind == crate::events::GATE_CACHE_BYPASSED)
    {
        if let Some(gate) = event
            .payload_json
            .as_deref()
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
            .and_then(|value| value.get("gate")?.as_str().map(str::to_owned))
        {
            *bypasses.entry(gate).or_default() += 1;
        }
    }
    for (gate, count) in bypasses {
        if count < RECOMMENDATION_MIN_SAMPLES {
            continue;
        }
        recommendations.push(gate_recommendation(
            RecommendationKind::RepeatedCacheBypass,
            AdvisoryProducer::GateReliabilityHistory,
            &gate,
            count,
            events.len().min(RECOMMENDATION_GATE_HISTORY_LIMIT),
            RecommendationConfidence::High,
            vec![
                evidence("gate", &safe_label(&gate)),
                evidence("cache_bypasses", &count.to_string()),
            ],
            "review why fresh execution is repeatedly required before changing cache policy",
        ));
    }
    recommendations.sort_by(|left, right| left.identity.cmp(&right.identity));
    recommendations
}

pub fn derive_isolation_resource_recommendations(
    events: &[Event],
    results: &[GateResult],
    leases: &[Lease],
    snapshot_time_ms: i64,
) -> Vec<MaintainerRecommendation> {
    let events = &events[..events.len().min(RECOMMENDATION_GATE_HISTORY_LIMIT)];
    let mut recommendations = Vec::new();
    recommendations.extend(path_event_recommendations(
        events,
        crate::events::GUARD_OUT_OF_LEASE_WRITE,
        RecommendationKind::OutOfLeaseWrites,
        AdvisoryProducer::IsolationHistory,
        "declare exact planned paths before broad rewrite commands",
    ));
    recommendations.extend(path_event_recommendations(
        events,
        crate::events::GUARD_UNTRACKED_ARTIFACT,
        RecommendationKind::UntrackedRuntimeArtifacts,
        AdvisoryProducer::IsolationHistory,
        "redirect runtime output into ignored per-worker paths or declare generated ownership",
    ));

    let mut contention = BTreeMap::<String, usize>::new();
    for result in results.iter().take(RECOMMENDATION_GATE_HISTORY_LIMIT) {
        if result.failure_class == Some(GateFailureClass::ResourceContention) {
            *contention.entry(result.gate_name.clone()).or_default() += 1;
        }
    }
    for (gate, count) in contention {
        if count >= RECOMMENDATION_MIN_SAMPLES {
            recommendations.push(gate_recommendation(
                RecommendationKind::ResourceContention,
                AdvisoryProducer::ResourceHistory,
                &gate,
                count,
                results.len().min(RECOMMENDATION_GATE_HISTORY_LIMIT),
                RecommendationConfidence::High,
                vec![
                    evidence("gate", &safe_label(&gate)),
                    evidence("resource_contention_failures", &count.to_string()),
                ],
                "review resource capacity, wait policy, and per-worker isolation",
            ));
        }
    }

    let mut expired = BTreeMap::<String, usize>::new();
    for lease in leases.iter().take(RECOMMENDATION_HISTORY_LIMIT) {
        if lease
            .expires_at
            .is_some_and(|expiry| expiry <= snapshot_time_ms)
            && repository_relative(&lease.path)
        {
            *expired.entry(lease.path.clone()).or_default() += 1;
        }
    }
    for (path, count) in expired {
        if count < RECOMMENDATION_MIN_SAMPLES {
            continue;
        }
        recommendations.push(MaintainerRecommendation {
            identity: stable_identity(RecommendationKind::LeaseExpiration, &path),
            audience: AdvisoryAudience::Maintainer,
            producer: AdvisoryProducer::ResourceHistory,
            kind: RecommendationKind::LeaseExpiration,
            severity: AdvisorySeverity::Warning,
            confidence: RecommendationConfidence::High,
            sample_count: count,
            window_size: leases.len().min(RECOMMENDATION_HISTORY_LIMIT),
            paths: vec![path],
            evidence: vec![evidence("expired_leases", &count.to_string())],
            remediation:
                "increase or heartbeat the lease only if the owning work routinely remains active"
                    .into(),
        });
    }

    recommendations.sort_by(|left, right| left.identity.cmp(&right.identity));
    recommendations
}

fn path_event_recommendations(
    events: &[Event],
    event_kind: &str,
    kind: RecommendationKind,
    producer: AdvisoryProducer,
    remediation: &str,
) -> Vec<MaintainerRecommendation> {
    let mut occurrences = BTreeMap::<String, usize>::new();
    for event in events.iter().filter(|event| event.kind == event_kind) {
        let paths = event
            .payload_json
            .as_deref()
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
            .and_then(|value| value.get("paths")?.as_array().cloned())
            .unwrap_or_default();
        for path in paths
            .iter()
            .filter_map(serde_json::Value::as_str)
            .filter(|path| repository_relative(path))
        {
            *occurrences.entry(path.to_owned()).or_default() += 1;
        }
    }
    occurrences
        .into_iter()
        .filter(|(_, count)| *count >= RECOMMENDATION_MIN_SAMPLES)
        .map(|(path, count)| MaintainerRecommendation {
            identity: stable_identity(kind, &path),
            audience: AdvisoryAudience::Maintainer,
            producer,
            kind,
            severity: AdvisorySeverity::Warning,
            confidence: RecommendationConfidence::High,
            sample_count: count,
            window_size: events.len(),
            paths: vec![path],
            evidence: vec![evidence("occurrences", &count.to_string())],
            remediation: remediation.into(),
        })
        .collect()
}

fn gate_recommendation(
    kind: RecommendationKind,
    producer: AdvisoryProducer,
    gate: &str,
    sample_count: usize,
    window_size: usize,
    confidence: RecommendationConfidence,
    evidence: Vec<AdvisoryEvidence>,
    remediation: &str,
) -> MaintainerRecommendation {
    MaintainerRecommendation {
        identity: stable_identity(kind, gate),
        audience: AdvisoryAudience::Maintainer,
        producer,
        kind,
        severity: AdvisorySeverity::Warning,
        confidence,
        sample_count,
        window_size,
        paths: Vec::new(),
        evidence,
        remediation: remediation.into(),
    }
}

fn evidence(kind: &str, summary: &str) -> AdvisoryEvidence {
    AdvisoryEvidence {
        kind: kind.into(),
        summary: summary.into(),
    }
}

fn safe_label(label: &str) -> String {
    let lower = label.to_ascii_lowercase();
    if label.is_empty()
        || label.len() > 128
        || label.chars().any(char::is_control)
        || ["secret", "token", "password", "credential"]
            .iter()
            .any(|needle| lower.contains(needle))
    {
        "redacted-gate".into()
    } else {
        label.to_owned()
    }
}

fn conflict_paths(details: Option<&str>) -> Vec<String> {
    let Some(details) = details else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(details) else {
        return Vec::new();
    };
    let mut paths = value
        .get("conflicts")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter(|path| repository_relative(path))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths.truncate(32);
    paths
}

pub(crate) fn repository_relative(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4_096
        && !path.contains('\0')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

pub(crate) fn stable_identity(kind: RecommendationKind, scope: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(scope.as_bytes()));
    format!("history:{}:{}", kind.as_str(), &digest[..24])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: i64, status: MergeStatus, details: Option<&str>) -> MergeQueueEntry {
        MergeQueueEntry {
            id,
            session_id: id,
            head_commit: format!("{id:040x}"),
            base_commit: "0".repeat(40),
            status,
            merged_tree: None,
            details_json: details.map(str::to_owned),
            created_at: id,
            updated_at: id,
        }
    }

    fn gate_result(
        id: i64,
        gate: &str,
        tree: &str,
        status: GateStatus,
        failure_class: Option<GateFailureClass>,
        duration_ms: i64,
    ) -> GateResult {
        GateResult {
            id,
            gate_name: gate.into(),
            tree_hash: tree.into(),
            definition_hash: "d".repeat(64),
            status,
            failure_class,
            exit_code: None,
            duration_ms: Some(duration_ms),
            wait_duration_ms: None,
            first_output_ms: None,
            output_bytes: None,
            log_path: Some("/absolute/log/must-not-leak".into()),
            session_id: Some(1),
            created_at: id,
        }
    }

    fn event(id: i64, gate: &str) -> Event {
        Event {
            id,
            schema_version: 1,
            ts: id,
            kind: crate::events::GATE_CACHE_BYPASSED.into(),
            session_id: Some(1),
            payload_json: Some(
                serde_json::json!({"gate": gate, "tree": "f".repeat(40)}).to_string(),
            ),
        }
    }

    fn path_event(id: i64, kind: &str, path: &str) -> Event {
        Event {
            id,
            schema_version: 1,
            ts: id,
            kind: kind.into(),
            session_id: Some(1),
            payload_json: Some(serde_json::json!({"paths": [path]}).to_string()),
        }
    }

    #[test]
    fn repeated_conflicts_are_bounded_deterministic_and_path_safe() {
        let entries = vec![
            entry(
                5,
                MergeStatus::Conflict,
                Some(r#"{"conflicts":["src/a.rs","/tmp/secret"]}"#),
            ),
            entry(4, MergeStatus::Promoted, Some(r#"{"diff":"SECRET"}"#)),
            entry(
                3,
                MergeStatus::Conflict,
                Some(r#"{"conflicts":["src/a.rs","../escape"]}"#),
            ),
            entry(
                2,
                MergeStatus::Conflict,
                Some(r#"{"conflicts":["src/a.rs"]}"#),
            ),
        ];

        let first = derive_conflict_recommendations(&entries);
        let second = derive_conflict_recommendations(&entries);
        assert_eq!(first, second);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].audience, AdvisoryAudience::Maintainer);
        assert_eq!(first[0].producer, AdvisoryProducer::ConflictHistory);
        assert_eq!(first[0].sample_count, 3);
        assert_eq!(first[0].paths, ["src/a.rs"]);
        assert!(
            first[0]
                .identity
                .starts_with("history:repeated_merge_conflict:")
        );
        let json = serde_json::to_string(&first).unwrap();
        for forbidden in ["SECRET", "/tmp/secret", "../escape", "diff", "task"] {
            assert!(!json.contains(forbidden), "leaked {forbidden:?}");
        }
    }

    #[test]
    fn conflict_recommendations_require_three_samples() {
        let entries = vec![
            entry(
                2,
                MergeStatus::Conflict,
                Some(r#"{"conflicts":["src/a.rs"]}"#),
            ),
            entry(
                1,
                MergeStatus::Conflict,
                Some(r#"{"conflicts":["src/a.rs"]}"#),
            ),
        ];
        assert!(derive_conflict_recommendations(&entries).is_empty());
    }

    #[test]
    fn gate_reliability_uses_tree_provenance_statistics_and_bypass_events() {
        let gate = "workspace";
        let mut merges = Vec::new();
        let mut results = Vec::new();
        for id in 1..=3 {
            let tree = format!("{id:040x}");
            let mut merge = entry(id, MergeStatus::Rejected, None);
            merge.merged_tree = Some(tree.clone());
            merges.push(merge);
            results.push(gate_result(
                id,
                gate,
                &tree,
                GateStatus::Error,
                Some(GateFailureClass::Environment),
                130_000,
            ));
            results.push(gate_result(
                id + 10,
                gate,
                &format!("{:040x}", id + 10),
                GateStatus::Pass,
                None,
                130_000,
            ));
        }
        for id in 20..=23 {
            results.push(gate_result(
                id,
                gate,
                &format!("{id:040x}"),
                GateStatus::Pass,
                None,
                130_000,
            ));
        }
        let events = vec![event(3, gate), event(2, gate), event(1, gate)];

        let first = derive_gate_recommendations(&results, &merges, &events);
        let second = derive_gate_recommendations(&results, &merges, &events);
        assert_eq!(first, second);
        let kinds = first.iter().map(|item| item.kind).collect::<BTreeSet<_>>();
        assert_eq!(
            kinds,
            BTreeSet::from([
                RecommendationKind::SessionPassMergedFail,
                RecommendationKind::RepeatedInfrastructureFailure,
                RecommendationKind::SlowGate,
                RecommendationKind::RepeatedCacheBypass,
            ])
        );
        let json = serde_json::to_string(&first).unwrap();
        assert!(!json.contains("/absolute/log"));
        assert!(!json.contains("definition_hash"));
        assert!(!json.contains("output"));
    }

    #[test]
    fn duration_recommendations_require_sufficient_samples() {
        let results = (1..RECOMMENDATION_DURATION_MIN_SAMPLES as i64)
            .map(|id| {
                gate_result(
                    id,
                    "slow",
                    &format!("{id:040x}"),
                    GateStatus::Pass,
                    None,
                    1_000_000,
                )
            })
            .collect::<Vec<_>>();
        assert!(derive_gate_recommendations(&results, &[], &[]).is_empty());
    }

    #[test]
    fn isolation_and_resource_history_is_bounded_typed_and_private() {
        let mut events = Vec::new();
        for id in 1..=3 {
            events.push(path_event(
                id,
                crate::events::GUARD_OUT_OF_LEASE_WRITE,
                "src/generated.rs",
            ));
            events.push(path_event(
                id + 10,
                crate::events::GUARD_UNTRACKED_ARTIFACT,
                "tmp/runtime.sock",
            ));
        }
        events.push(path_event(
            99,
            crate::events::GUARD_UNTRACKED_ARTIFACT,
            "/tmp/absolute-secret",
        ));
        let results = (1..=3)
            .map(|id| {
                gate_result(
                    id,
                    "database",
                    &format!("{id:040x}"),
                    GateStatus::Error,
                    Some(GateFailureClass::ResourceContention),
                    10,
                )
            })
            .collect::<Vec<_>>();
        let leases = (1..=3)
            .map(|id| Lease {
                id,
                session_id: id,
                path: "database/".into(),
                kind: crate::LeaseKind::Explicit,
                created_at: 1,
                expires_at: Some(5),
                released_at: None,
            })
            .collect::<Vec<_>>();

        let recommendations =
            derive_isolation_resource_recommendations(&events, &results, &leases, 10);
        let kinds = recommendations
            .iter()
            .map(|item| item.kind)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            kinds,
            BTreeSet::from([
                RecommendationKind::OutOfLeaseWrites,
                RecommendationKind::UntrackedRuntimeArtifacts,
                RecommendationKind::ResourceContention,
                RecommendationKind::LeaseExpiration,
            ])
        );
        let json = serde_json::to_string(&recommendations).unwrap();
        assert!(!json.contains("/tmp/absolute-secret"));
        assert!(!json.contains("printf"));
        assert!(!json.contains("SECRET_OUTPUT"));
        assert!(!json.contains("environment"));
    }
}
