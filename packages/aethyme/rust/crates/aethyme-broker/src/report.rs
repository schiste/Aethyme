//! Pure, allowlist-only inputs and outputs for local broker reports.
//!
//! The builder deliberately accepts rich broker rows but constructs a new
//! report schema field by field. Sensitive source fields therefore cannot
//! enter a snapshot through generic serialization or JSON pass-through.

use std::cmp::Ordering;
use std::path::{Component, Path};

use crate::gates::GateRunOutcome;
use crate::types::{
    CoordinatedOperation, Event, GateFailureClass, GateStatus, OperationEffect, OperationProvider,
    OperationStatus, Session, SessionOrigin, SessionStatus,
};
use crate::version::BinaryBuild;

pub const REPORT_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const REPORT_RECENT_EVENT_LIMIT: usize = 20;
pub const REPORT_RECENT_OPERATION_LIMIT: usize = 20;
pub const REPORT_RECENT_GATE_LIMIT: usize = 20;

/// One observed gate result plus the provenance not carried by
/// [`GateRunOutcome`] itself.
#[derive(Debug, Clone, Copy)]
pub struct ReportGateObservation<'a> {
    pub outcome: &'a GateRunOutcome,
    /// Unix epoch milliseconds.
    pub recorded_at: i64,
    /// Relevant gate-trigger path, when known. Unsafe or absolute spellings
    /// are omitted by the builder.
    pub triggered_by: Option<&'a str>,
}

/// Pure builder for a redacted report snapshot.
///
/// It performs no filesystem, environment, database, Git, or network access.
/// Capture layers gather rows and platform strings; this type only projects
/// them through the explicit report allowlist.
pub struct ReportSnapshotBuilder<'a> {
    build: &'a BinaryBuild,
    os: &'a str,
    arch: &'a str,
    session: Option<&'a Session>,
    events: &'a [Event],
    operations: &'a [CoordinatedOperation],
    gates: &'a [ReportGateObservation<'a>],
    include_task: bool,
}

impl<'a> ReportSnapshotBuilder<'a> {
    pub fn new(build: &'a BinaryBuild, os: &'a str, arch: &'a str) -> Self {
        Self {
            build,
            os,
            arch,
            session: None,
            events: &[],
            operations: &[],
            gates: &[],
            include_task: false,
        }
    }

    pub fn session(mut self, session: &'a Session) -> Self {
        self.session = Some(session);
        self
    }

    pub fn recent_events(mut self, events: &'a [Event]) -> Self {
        self.events = events;
        self
    }

    pub fn operations(mut self, operations: &'a [CoordinatedOperation]) -> Self {
        self.operations = operations;
        self
    }

    pub fn gate_observations(mut self, gates: &'a [ReportGateObservation<'a>]) -> Self {
        self.gates = gates;
        self
    }

    /// Opt in to task text and coordinated-operation authorization reasons.
    /// Future CLI capture must wire this only from an explicit
    /// `--include-task` invocation flag.
    pub fn include_task(mut self, include: bool) -> Self {
        self.include_task = include;
        self
    }

    pub fn build(self) -> ReportSnapshot {
        let session = self.session.map(|session| ReportSession {
            id: session.id,
            branch: session.branch.clone(),
            origin: session.origin,
            status: session.status,
            diff_base: session.diff_base.clone(),
            task: self.include_task.then(|| session.task.clone()).flatten(),
        });

        let mut recent_event_types = self
            .events
            .iter()
            .filter(|event| is_safe_event_kind(&event.kind))
            .map(|event| ReportEventType {
                id: event.id,
                recorded_at: event.ts,
                kind: event.kind.clone(),
                session_id: event.session_id,
            })
            .collect::<Vec<_>>();
        recent_event_types.sort_by(|left, right| {
            right
                .id
                .cmp(&left.id)
                .then_with(|| right.recorded_at.cmp(&left.recorded_at))
                .then_with(|| left.kind.cmp(&right.kind))
        });
        recent_event_types.truncate(REPORT_RECENT_EVENT_LIMIT);

        let mut operations = self
            .operations
            .iter()
            .map(|operation| ReportOperation {
                id: operation.id,
                session_id: operation.session_id,
                provider: operation.provider,
                repository: operation.repository.clone(),
                effect: operation.effect,
                status: operation.status,
                exit_code: operation.exit_code,
                started_at: operation.created_at,
                finished_at: operation.finished_at,
                authorization_reason: self
                    .include_task
                    .then(|| operation.authorization_reason.clone())
                    .flatten(),
            })
            .collect::<Vec<_>>();
        operations.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        operations.truncate(REPORT_RECENT_OPERATION_LIMIT);

        let mut gates = self
            .gates
            .iter()
            .map(|observation| ReportGateProvenance {
                gate: observation.outcome.gate.clone(),
                tree_hash: observation.outcome.tree_hash.clone(),
                status: observation.outcome.status,
                failure_class: observation.outcome.failure_class,
                cache_source: if observation.outcome.cached {
                    ReportGateCacheSource::CacheHit
                } else {
                    ReportGateCacheSource::Executed
                },
                exit_code: observation.outcome.exit_code,
                duration_ms: observation.outcome.duration_ms,
                recorded_at: observation.recorded_at,
                triggered_by: observation
                    .triggered_by
                    .filter(|path| is_safe_repo_relative(path))
                    .map(str::to_string),
            })
            .collect::<Vec<_>>();
        gates.sort_by(|left, right| {
            right
                .recorded_at
                .cmp(&left.recorded_at)
                .then_with(|| left.gate.cmp(&right.gate))
                .then_with(|| left.tree_hash.cmp(&right.tree_hash))
        });
        gates.truncate(REPORT_RECENT_GATE_LIMIT);

        let last_known_failure = last_known_failure(self.session, self.operations, self.gates);

        ReportSnapshot {
            schema_version: REPORT_SNAPSHOT_SCHEMA_VERSION,
            includes_task: self.include_task,
            build: ReportBuild {
                version: self.build.version.clone(),
                commit: self.build.commit.clone(),
            },
            platform: ReportPlatform {
                os: self.os.to_string(),
                arch: self.arch.to_string(),
            },
            session,
            recent_event_types,
            operations,
            gates,
            last_known_failure,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReportSnapshot {
    pub schema_version: u32,
    pub includes_task: bool,
    pub build: ReportBuild,
    pub platform: ReportPlatform,
    pub session: Option<ReportSession>,
    pub recent_event_types: Vec<ReportEventType>,
    pub operations: Vec<ReportOperation>,
    pub gates: Vec<ReportGateProvenance>,
    pub last_known_failure: Option<ReportLastFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReportBuild {
    pub version: String,
    pub commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReportPlatform {
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReportSession {
    pub id: i64,
    pub branch: String,
    pub origin: SessionOrigin,
    pub status: SessionStatus,
    pub diff_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReportEventType {
    pub id: i64,
    pub recorded_at: i64,
    pub kind: String,
    pub session_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReportOperation {
    pub id: i64,
    pub session_id: i64,
    pub provider: OperationProvider,
    pub repository: String,
    pub effect: OperationEffect,
    pub status: OperationStatus,
    pub exit_code: Option<i64>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportGateCacheSource {
    Executed,
    CacheHit,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReportGateProvenance {
    pub gate: String,
    pub tree_hash: String,
    pub status: GateStatus,
    pub failure_class: Option<GateFailureClass>,
    pub cache_source: ReportGateCacheSource,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub recorded_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triggered_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ReportLastFailure {
    Session {
        session_id: i64,
        recorded_at: i64,
        exit_code: i64,
    },
    Operation {
        operation_id: i64,
        recorded_at: i64,
        status: OperationStatus,
        exit_code: Option<i64>,
    },
    Gate {
        gate: String,
        tree_hash: String,
        recorded_at: i64,
        status: GateStatus,
        failure_class: Option<GateFailureClass>,
        exit_code: Option<i64>,
        cache_source: ReportGateCacheSource,
    },
}

struct FailureCandidate {
    recorded_at: i64,
    source_rank: u8,
    stable_key: String,
    failure: ReportLastFailure,
}

fn last_known_failure(
    session: Option<&Session>,
    operations: &[CoordinatedOperation],
    gates: &[ReportGateObservation<'_>],
) -> Option<ReportLastFailure> {
    let mut candidates = Vec::new();
    if let Some(session) = session
        && let Some(exit_code) = session.exit_code.filter(|code| *code != 0)
    {
        candidates.push(FailureCandidate {
            recorded_at: session.updated_at,
            source_rank: 0,
            stable_key: session.id.to_string(),
            failure: ReportLastFailure::Session {
                session_id: session.id,
                recorded_at: session.updated_at,
                exit_code,
            },
        });
    }

    for operation in operations {
        if !operation_failed(operation) {
            continue;
        }
        let recorded_at = operation.finished_at.unwrap_or(operation.updated_at);
        candidates.push(FailureCandidate {
            recorded_at,
            source_rank: 1,
            stable_key: operation.id.to_string(),
            failure: ReportLastFailure::Operation {
                operation_id: operation.id,
                recorded_at,
                status: operation.status,
                exit_code: operation.exit_code,
            },
        });
    }

    for observation in gates {
        if !gate_failed(observation.outcome) {
            continue;
        }
        candidates.push(FailureCandidate {
            recorded_at: observation.recorded_at,
            source_rank: 2,
            stable_key: format!(
                "{}:{}",
                observation.outcome.gate, observation.outcome.tree_hash
            ),
            failure: ReportLastFailure::Gate {
                gate: observation.outcome.gate.clone(),
                tree_hash: observation.outcome.tree_hash.clone(),
                recorded_at: observation.recorded_at,
                status: observation.outcome.status,
                failure_class: observation.outcome.failure_class,
                exit_code: observation.outcome.exit_code,
                cache_source: if observation.outcome.cached {
                    ReportGateCacheSource::CacheHit
                } else {
                    ReportGateCacheSource::Executed
                },
            },
        });
    }

    candidates
        .into_iter()
        .max_by(compare_failure_candidates)
        .map(|candidate| candidate.failure)
}

fn compare_failure_candidates(left: &FailureCandidate, right: &FailureCandidate) -> Ordering {
    left.recorded_at
        .cmp(&right.recorded_at)
        .then_with(|| left.source_rank.cmp(&right.source_rank))
        .then_with(|| left.stable_key.cmp(&right.stable_key))
}

fn operation_failed(operation: &CoordinatedOperation) -> bool {
    operation.exit_code.is_some_and(|code| code != 0)
        || matches!(
            operation.status,
            OperationStatus::Failed
                | OperationStatus::OutcomeUnknown
                | OperationStatus::ReconciledFailed
        )
}

fn gate_failed(outcome: &GateRunOutcome) -> bool {
    outcome.exit_code.is_some_and(|code| code != 0)
        || matches!(outcome.status, GateStatus::Fail | GateStatus::Error)
}

fn is_safe_event_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_safe_repo_relative(path: &str) -> bool {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.bytes().any(|byte| byte.is_ascii_control())
        || looks_like_windows_absolute(path)
    {
        return false;
    }
    !Path::new(path).is_absolute()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn looks_like_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn build() -> BinaryBuild {
        BinaryBuild {
            version: "0.1.5".into(),
            describe: Some("v0.1.5-1-gabc".into()),
            commit: Some("abcdef0123456789".into()),
            path: Some("/private/bin/aethyme-BINARY-PATH-SECRET".into()),
        }
    }

    fn session() -> Session {
        Session {
            id: 42,
            worktree_path: "/private/repo/WORKTREE-PATH-SECRET".into(),
            branch: "agent/report-capture".into(),
            origin: SessionOrigin::Adopted,
            status: SessionStatus::Exited,
            task: Some("TASK-TEXT-SECRET".into()),
            diff_base: Some("0123456789abcdef".into()),
            pid: Some(999),
            command: Some("COMMAND-SECRET --token TOKEN-SECRET".into()),
            log_path: Some("/private/log/SESSION-LOG-SECRET".into()),
            exit_code: Some(17),
            created_at: 100,
            updated_at: 500,
            last_activity_at: 490,
        }
    }

    fn event(id: i64, kind: &str, payload: &str) -> Event {
        Event {
            id,
            schema_version: 1,
            ts: id * 10,
            kind: kind.into(),
            session_id: Some(42),
            payload_json: Some(payload.into()),
        }
    }

    fn operation() -> CoordinatedOperation {
        CoordinatedOperation {
            id: 7,
            session_id: 42,
            provider: OperationProvider::Github,
            repository: "owner/repo".into(),
            scope: "/private/repo/OPERATION-SCOPE-PATH-SECRET".into(),
            effect: OperationEffect::Write,
            status: OperationStatus::Failed,
            authorization_reason: Some("OPERATION-REASON-SECRET".into()),
            command_json: r#"["issue","create","FILE-CONTENT-SECRET"]"#.into(),
            pid: 1234,
            exit_code: Some(2),
            details_json: Some(r#"{"diff":"DIFF-SECRET","hunk":"HUNK-SECRET"}"#.into()),
            created_at: 200,
            updated_at: 300,
            finished_at: Some(300),
        }
    }

    fn gate() -> GateRunOutcome {
        GateRunOutcome {
            gate: "cargo-test".into(),
            tree_hash: "fedcba9876543210".into(),
            status: GateStatus::Fail,
            failure_class: Some(GateFailureClass::TestFailure),
            cached: false,
            exit_code: Some(101),
            duration_ms: Some(1200),
            log_path: Some("/private/log/GATE-LOG-SECRET".into()),
        }
    }

    #[test]
    fn default_snapshot_is_an_explicit_forbidden_field_boundary() {
        let build = build();
        let session = session();
        let events = vec![event(
            8,
            "gate.failed",
            r#"{"content":"FILE-CONTENT-SECRET","diff":"DIFF-SECRET","hunk":"HUNK-SECRET"}"#,
        )];
        let operations = vec![operation()];
        let gate = gate();
        let gates = vec![ReportGateObservation {
            outcome: &gate,
            recorded_at: 400,
            triggered_by: Some("/private/repo/ABSOLUTE-TRIGGER-PATH-SECRET"),
        }];

        let snapshot = ReportSnapshotBuilder::new(&build, "linux", "x86_64")
            .session(&session)
            .recent_events(&events)
            .operations(&operations)
            .gate_observations(&gates)
            .build();
        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        let value = serde_json::to_value(&snapshot).unwrap();

        for forbidden in [
            "BINARY-PATH-SECRET",
            "WORKTREE-PATH-SECRET",
            "TASK-TEXT-SECRET",
            "COMMAND-SECRET",
            "TOKEN-SECRET",
            "SESSION-LOG-SECRET",
            "OPERATION-SCOPE-PATH-SECRET",
            "OPERATION-REASON-SECRET",
            "FILE-CONTENT-SECRET",
            "DIFF-SECRET",
            "HUNK-SECRET",
            "GATE-LOG-SECRET",
            "ABSOLUTE-TRIGGER-PATH-SECRET",
        ] {
            assert!(
                !json.contains(forbidden),
                "leaked forbidden value {forbidden}"
            );
        }
        assert!(!snapshot.includes_task);
        assert_eq!(snapshot.session.as_ref().unwrap().task, None);
        assert_eq!(snapshot.operations[0].authorization_reason, None);
        assert_eq!(snapshot.gates[0].triggered_by, None);

        let root_keys = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            root_keys,
            BTreeSet::from([
                "build",
                "gates",
                "includes_task",
                "last_known_failure",
                "operations",
                "platform",
                "recent_event_types",
                "schema_version",
                "session",
            ])
        );
        let session_keys = value["session"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            session_keys,
            BTreeSet::from(["branch", "diff_base", "id", "origin", "status"])
        );
        let operation_keys = value["operations"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            operation_keys,
            BTreeSet::from([
                "effect",
                "exit_code",
                "finished_at",
                "id",
                "provider",
                "repository",
                "session_id",
                "started_at",
                "status",
            ])
        );
    }

    #[test]
    fn include_task_is_the_only_task_and_reason_opt_in() {
        let build = build();
        let session = session();
        let operations = vec![operation()];
        let snapshot = ReportSnapshotBuilder::new(&build, "macos", "aarch64")
            .session(&session)
            .operations(&operations)
            .include_task(true)
            .build();

        assert!(snapshot.includes_task);
        assert_eq!(
            snapshot.session.unwrap().task.as_deref(),
            Some("TASK-TEXT-SECRET")
        );
        assert_eq!(
            snapshot.operations[0].authorization_reason.as_deref(),
            Some("OPERATION-REASON-SECRET")
        );
    }

    #[test]
    fn snapshot_is_bounded_and_deterministic_with_relative_paths_only() {
        let build = build();
        let events = (1..=REPORT_RECENT_EVENT_LIMIT as i64 + 5)
            .rev()
            .map(|id| event(id, "gate.passed", "PAYLOAD-SECRET"))
            .collect::<Vec<_>>();
        let first_gate = GateRunOutcome {
            status: GateStatus::Pass,
            cached: true,
            exit_code: Some(0),
            ..gate()
        };
        let second_gate = GateRunOutcome {
            gate: "lint".into(),
            tree_hash: "aaaaaaaaaaaaaaaa".into(),
            status: GateStatus::Pass,
            failure_class: None,
            cached: false,
            exit_code: Some(0),
            duration_ms: Some(10),
            log_path: None,
        };
        let gates = vec![
            ReportGateObservation {
                outcome: &first_gate,
                recorded_at: 100,
                triggered_by: Some("src/lib.rs"),
            },
            ReportGateObservation {
                outcome: &second_gate,
                recorded_at: 100,
                triggered_by: Some("../outside.rs"),
            },
        ];

        let make = || {
            ReportSnapshotBuilder::new(&build, "linux", "x86_64")
                .recent_events(&events)
                .gate_observations(&gates)
                .build()
        };
        let first = make();
        let second = make();

        assert_eq!(first, second);
        assert_eq!(first.recent_event_types.len(), REPORT_RECENT_EVENT_LIMIT);
        assert_eq!(first.recent_event_types[0].id, 25);
        assert_eq!(first.recent_event_types.last().unwrap().id, 6);
        assert_eq!(first.gates[0].gate, "cargo-test");
        assert_eq!(first.gates[0].triggered_by.as_deref(), Some("src/lib.rs"));
        assert_eq!(first.gates[1].triggered_by, None);
    }

    #[test]
    fn last_known_failure_uses_latest_timestamp_without_failure_text() {
        let build = build();
        let session = session();
        let operations = vec![operation()];
        let gate = gate();
        let gates = vec![ReportGateObservation {
            outcome: &gate,
            recorded_at: 400,
            triggered_by: None,
        }];

        let snapshot = ReportSnapshotBuilder::new(&build, "linux", "x86_64")
            .session(&session)
            .operations(&operations)
            .gate_observations(&gates)
            .build();

        assert_eq!(
            snapshot.last_known_failure,
            Some(ReportLastFailure::Session {
                session_id: 42,
                recorded_at: 500,
                exit_code: 17,
            })
        );
    }
}
