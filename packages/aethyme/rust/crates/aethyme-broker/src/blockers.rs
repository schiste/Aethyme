//! One registry for "why is this repository blocked right now, and what is
//! the one command that clears it".
//!
//! Coordination state lives in several stores that each grew their own ids,
//! healing rules and recovery verbs: the repository's `broker.db`, the
//! machine-wide `host-operations.db` and `host-resources.db`, gate pidfiles,
//! and the broker-action-required notice. Every one of them was a correct
//! barrier; what was missing was a single place that names all of them at
//! once, so a killed write that leaves two blockers (#286) is seen as two, and
//! an id printed by one store is accepted by the command that clears it
//! (#276).
//!
//! Collection is read-only. Clearing dispatches to the existing recovery code
//! for each kind wherever one exists, and refuses -- with the reason and the
//! flag it needs -- anything that could lose work or that needs an outcome
//! decision only an operator can make.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::{Broker, BrokerOpError, GitRepo, OperationStatus, SessionStatus};

/// Event recorded whenever `broker unblock` clears something.
pub const BLOCKER_CLEARED: &str = "broker.blocker.cleared";

/// How many cached failing verdicts one listing carries.
const GATE_CACHE_LIMIT: i64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerKind {
    /// A repository coordinated operation in `outcome_unknown`.
    Operation,
    /// A host-ledger operation in `outcome_unknown`, or running with a dead
    /// holder, that no repository operation row accounts for.
    HostOperation,
    /// A quarantined host resource lease for this repository.
    ResourceLease,
    /// An explicit path lease held by a stale session.
    PathLease,
    /// A conclusive failing gate verdict the cache will reuse.
    GateCache,
    /// A gate pidfile whose process is gone.
    Pidfile,
    /// A `.aethyme/broker-action-required.md` notice in a session worktree.
    ActionRequired,
}

impl BlockerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operation => "operation",
            Self::HostOperation => "host_operation",
            Self::ResourceLease => "resource_lease",
            Self::PathLease => "path_lease",
            Self::GateCache => "gate_cache",
            Self::Pidfile => "pidfile",
            Self::ActionRequired => "action_required",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerScope {
    Repo,
    Host,
}

/// One current blocker, in one id namespace across every store.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Blocker {
    /// Stable id accepted by `aethyme broker unblock`.
    pub id: String,
    pub kind: BlockerKind,
    pub scope: BlockerScope,
    pub cause: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<i64>,
    /// The exact command that clears it.
    pub clear: String,
    pub safe_to_clear_automatically: bool,
}

/// A store the collection could not read. Reported rather than dropped: an
/// empty list must never mean "nothing blocks" when part of it was not read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BlockerSourceError {
    pub source: &'static str,
    pub error: String,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BlockerReport {
    pub blockers: Vec<Blocker>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unavailable: Vec<BlockerSourceError>,
}

/// A parsed blocker id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockerRef {
    Operation(i64),
    HostOperation(String),
    ResourceLease(String),
    PathLease(i64),
    GateCache { gate: String, tree: String },
    Pidfile { session_id: i64, gate: String },
    ActionRequired(i64),
}

impl BlockerRef {
    pub fn parse(id: &str) -> Result<Self, String> {
        let (prefix, rest) = id.split_once(':').ok_or_else(|| {
            format!("blocker id {id:?} must be <kind>:<key>; run `aethyme broker blockers`")
        })?;
        let positive = |value: &str| {
            value
                .parse::<i64>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    format!("blocker id {id:?} needs a positive integer after {prefix}:")
                })
        };
        match prefix {
            "op" => positive(rest).map(Self::Operation),
            "hostop" => {
                if rest.len() == 32 && rest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    Ok(Self::HostOperation(rest.to_ascii_lowercase()))
                } else {
                    Err(format!(
                        "blocker id {id:?}: a host operation id is 32 hexadecimal characters"
                    ))
                }
            }
            "resource" if !rest.is_empty() => Ok(Self::ResourceLease(rest.into())),
            "lease" => positive(rest).map(Self::PathLease),
            "gatecache" => {
                let (gate, tree) = rest
                    .rsplit_once('@')
                    .filter(|(gate, tree)| !gate.is_empty() && !tree.is_empty())
                    .ok_or_else(|| format!("blocker id {id:?} must be gatecache:<gate>@<tree>"))?;
                Ok(Self::GateCache {
                    gate: gate.into(),
                    tree: tree.into(),
                })
            }
            "pidfile" => {
                let (session, gate) = rest
                    .split_once('-')
                    .filter(|(_, gate)| !gate.is_empty())
                    .ok_or_else(|| format!("blocker id {id:?} must be pidfile:<session>-<gate>"))?;
                Ok(Self::Pidfile {
                    session_id: positive(session)?,
                    gate: gate.into(),
                })
            }
            "action" => positive(rest).map(Self::ActionRequired),
            _ => Err(format!(
                "unknown blocker kind {prefix:?}; expected op, hostop, resource, lease, gatecache, pidfile, or action"
            )),
        }
    }

    pub fn kind(&self) -> BlockerKind {
        match self {
            Self::Operation(_) => BlockerKind::Operation,
            Self::HostOperation(_) => BlockerKind::HostOperation,
            Self::ResourceLease(_) => BlockerKind::ResourceLease,
            Self::PathLease(_) => BlockerKind::PathLease,
            Self::GateCache { .. } => BlockerKind::GateCache,
            Self::Pidfile { .. } => BlockerKind::Pidfile,
            Self::ActionRequired(_) => BlockerKind::ActionRequired,
        }
    }
}

/// Operator-supplied facts for `broker unblock`. None of them is ever guessed.
#[derive(Debug, Clone, Default)]
pub struct UnblockRequest {
    pub id: String,
    /// `Some(true)` for `--outcome succeeded`, `Some(false)` for `failed`.
    pub outcome: Option<bool>,
    pub reason: Option<String>,
    pub confirm: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UnblockReport {
    pub id: String,
    pub kind: BlockerKind,
    pub cleared: bool,
    /// What was done, in the words of the recovery path that did it.
    pub action: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UnblockRefusal {
    pub id: String,
    pub kind: BlockerKind,
    pub cleared: bool,
    pub reason: String,
    /// Flags the operator must add, when supplying them would be enough.
    pub required_flags: Vec<String>,
    /// True when the refusal is for want of an outcome decision.
    pub needs_outcome: bool,
}

#[derive(Debug, Clone)]
pub enum UnblockOutcome {
    Cleared(UnblockReport),
    Refused(UnblockRefusal),
}

impl Broker {
    /// Every current blocker for this repository, read-only.
    pub fn blockers(&self) -> BlockerReport {
        let mut report = BlockerReport::default();
        let mut linked_host_operations = Vec::new();
        match self.operation_blockers(&mut linked_host_operations) {
            Ok(found) => report.blockers.extend(found),
            Err(error) => report
                .unavailable
                .push(source_error("broker.db operations", error)),
        }
        match self.host_operation_blockers(&linked_host_operations) {
            Ok(found) => report.blockers.extend(found),
            Err(error) => report
                .unavailable
                .push(source_error("host-operations.db", error)),
        }
        match self.resource_lease_blockers() {
            Ok(found) => report.blockers.extend(found),
            Err(error) => report
                .unavailable
                .push(source_error("host-resources.db", error)),
        }
        match self.path_lease_blockers() {
            Ok(found) => report.blockers.extend(found),
            Err(error) => report
                .unavailable
                .push(source_error("broker.db leases", error)),
        }
        match self.gate_cache_blockers() {
            Ok(found) => report.blockers.extend(found),
            Err(error) => report
                .unavailable
                .push(source_error("broker.db gate_results", error)),
        }
        report.blockers.extend(self.pidfile_blockers());
        match self.action_required_blockers() {
            Ok(found) => report.blockers.extend(found),
            Err(error) => report
                .unavailable
                .push(source_error("session worktrees", error)),
        }
        report
    }

    /// Clear one blocker through the recovery path its store already has.
    pub fn unblock(&mut self, request: &UnblockRequest) -> Result<UnblockOutcome, BrokerOpError> {
        let target = BlockerRef::parse(&request.id)
            .map_err(|reason| BrokerOpError::InvalidCoordinatedOperation { reason })?;
        match &target {
            BlockerRef::Operation(id) => self.unblock_operation(request, *id),
            BlockerRef::HostOperation(hex) => self.unblock_host_operation(request, hex),
            BlockerRef::ResourceLease(lease_id) => self.unblock_resource_lease(request, lease_id),
            BlockerRef::PathLease(lease_id) => self.unblock_path_lease(request, *lease_id),
            BlockerRef::GateCache { gate, tree } => self.unblock_gate_cache(request, gate, tree),
            BlockerRef::Pidfile { session_id, gate } => {
                self.unblock_pidfile(request, *session_id, gate)
            }
            BlockerRef::ActionRequired(session_id) => {
                let path = self.action_required_path(*session_id)?;
                Ok(refuse(
                    request,
                    BlockerKind::ActionRequired,
                    format!(
                        "session {session_id}'s submission conflicted; {} names the files and the \
                         rebase steps. Deleting the notice would hide the conflict, so resolve it \
                         and resubmit: aethyme broker submit --session {session_id}",
                        path.display()
                    ),
                    Vec::new(),
                    false,
                ))
            }
        }
    }

    // ── collection ──────────────────────────────────────────────────

    fn operation_blockers(&self, linked: &mut Vec<String>) -> Result<Vec<Blocker>, BrokerOpError> {
        let mut found = Vec::new();
        for operation in self.store_ref().pending_coordinated_operations()? {
            if operation.status != OperationStatus::OutcomeUnknown {
                continue;
            }
            let host = operation
                .host_operation_id
                .as_deref()
                .map(|hex| format!("; host operation {hex} is reconciled with it"))
                .unwrap_or_default();
            if let Some(hex) = &operation.host_operation_id {
                linked.push(hex.clone());
            }
            found.push(Blocker {
                id: format!("op:{}", operation.id),
                kind: BlockerKind::Operation,
                scope: BlockerScope::Repo,
                cause: format!(
                    "{} {} on {} ended with an unknown remote outcome; every remote write to it \
                     is blocked until an operator inspects the remote and records what happened{host}",
                    operation.provider.as_str(),
                    operation.effect.as_str(),
                    operation.repository,
                ),
                session_id: Some(operation.session_id),
                clear: format!("{} op:{} {OUTCOME_FLAGS}", UNBLOCK, operation.id),
                safe_to_clear_automatically: false,
            });
        }
        Ok(found)
    }

    fn host_operation_blockers(&self, linked: &[String]) -> Result<Vec<Blocker>, BrokerOpError> {
        let database = self.host_operation_database_path()?;
        let keys = self.repository_coordination_keys();
        let mut found = Vec::new();
        for row in unresolved_host_operations(&database).map_err(host_read_error)? {
            if linked.contains(&row.operation_id) {
                continue;
            }
            let relevant = keys.iter().any(|key| {
                row.remote_key == *key || row.remote_key.starts_with(&format!("{key}::"))
            });
            if !relevant {
                continue;
            }
            let holder_alive = process_alive(row.holder_pid);
            if row.status == "running" && holder_alive {
                // In progress, not blocked.
                continue;
            }
            found.push(Blocker {
                id: format!("hostop:{}", row.operation_id),
                kind: BlockerKind::HostOperation,
                scope: BlockerScope::Host,
                cause: format!(
                    "host operation for {} is {}{} and blocks every remote write to it from any \
                     clone on this host; its outcome must be decided from the remote",
                    row.remote_key,
                    row.status,
                    if holder_alive {
                        String::new()
                    } else {
                        format!(" (holder pid {} is gone)", row.holder_pid)
                    },
                ),
                session_id: None,
                clear: format!("{} hostop:{} {OUTCOME_FLAGS}", UNBLOCK, row.operation_id),
                safe_to_clear_automatically: false,
            });
        }
        Ok(found)
    }

    fn resource_lease_blockers(&self) -> Result<Vec<Blocker>, BrokerOpError> {
        let coordinator =
            crate::HostResourceCoordinator::open_read_only_default().map_err(|error| {
                BrokerOpError::InvalidCoordinatedOperation {
                    reason: error.to_string(),
                }
            })?;
        let repository = self.origin_fingerprint();
        let sessions = self.sessions_by_worktree_fingerprint()?;
        let mut found = Vec::new();
        for lease in
            coordinator
                .list(false)
                .map_err(|error| BrokerOpError::InvalidCoordinatedOperation {
                    reason: error.to_string(),
                })?
        {
            if lease.repository != repository
                && !sessions
                    .iter()
                    .any(|(fp, _)| *fp == lease.worktree_fingerprint)
            {
                continue;
            }
            if lease.state != crate::HostLeaseState::Quarantined {
                continue;
            }
            let owner = sessions
                .iter()
                .find(|(fp, _)| *fp == lease.worktree_fingerprint)
                .map(|(_, session)| session);
            let holder_alive = lease
                .holder_pid
                .is_some_and(|pid| process_alive(i64::from(pid)));
            let owner_closed = owner.is_some_and(|session| session.status.is_closed());
            let safe = !holder_alive && owner_closed;
            let allocations = lease
                .allocations
                .iter()
                .map(|allocation| format!("{}={}", allocation.key, allocation.value))
                .collect::<Vec<_>>()
                .join(", ");
            found.push(Blocker {
                id: format!("resource:{}", lease.lease_id),
                kind: BlockerKind::ResourceLease,
                scope: BlockerScope::Host,
                cause: format!(
                    "resource lease generation {} ({allocations}) is quarantined{}{}; it holds \
                     that capacity until it is reconciled",
                    lease.generation,
                    match lease.holder_pid {
                        Some(pid) if holder_alive => format!(", holder pid {pid} is still running"),
                        Some(pid) => format!(", holder pid {pid} is gone"),
                        None => String::new(),
                    },
                    match owner {
                        Some(session) => format!(
                            ", owned by session {} ({})",
                            session.id,
                            session.status.as_str()
                        ),
                        None => String::new(),
                    },
                ),
                session_id: owner.map(|session| session.id),
                clear: if safe {
                    format!("{UNBLOCK} resource:{}", lease.lease_id)
                } else {
                    format!(
                        "{UNBLOCK} resource:{} --confirm {}",
                        lease.lease_id, lease.generation
                    )
                },
                safe_to_clear_automatically: safe,
            });
        }
        Ok(found)
    }

    fn path_lease_blockers(&self) -> Result<Vec<Blocker>, BrokerOpError> {
        let mut found = Vec::new();
        for lease in self.store_ref().active_leases()? {
            if lease.kind != crate::LeaseKind::Explicit {
                continue;
            }
            let session = self.store_ref().session(lease.session_id)?;
            if session.status != SessionStatus::Stale {
                continue;
            }
            let worktree_gone = !Path::new(&session.worktree_path).exists();
            found.push(Blocker {
                id: format!("lease:{}", lease.id),
                kind: BlockerKind::PathLease,
                scope: BlockerScope::Repo,
                cause: format!(
                    "explicit lease on {} is held by stale session {}{}; no other session can \
                     claim it",
                    lease.path,
                    session.id,
                    if worktree_gone {
                        " whose worktree no longer exists"
                    } else {
                        ""
                    },
                ),
                session_id: Some(session.id),
                clear: if worktree_gone {
                    format!("{UNBLOCK} lease:{}", lease.id)
                } else {
                    format!("aethyme broker close --session {}", session.id)
                },
                safe_to_clear_automatically: worktree_gone,
            });
        }
        Ok(found)
    }

    fn gate_cache_blockers(&self) -> Result<Vec<Blocker>, BrokerOpError> {
        let database = crate::broker_db_path(self.main_root());
        if !database.exists() {
            return Ok(Vec::new());
        }
        let conn = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(sqlite_error)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(sqlite_error)?;
        let mut stmt = conn
            .prepare(
                "SELECT r.gate_name, r.tree_hash, r.session_id, r.id
                 FROM gate_results r
                 JOIN sessions s ON s.id = r.session_id AND s.cleanup_state = 'open'
                 WHERE r.status = 'fail' AND r.failure_class = 'test_failure'
                   AND NOT EXISTS (
                       SELECT 1 FROM gate_results p
                       WHERE p.gate_name = r.gate_name AND p.tree_hash = r.tree_hash
                         AND p.definition_hash = r.definition_hash AND p.id > r.id
                         AND (p.status = 'pass'
                              OR (p.status = 'fail' AND p.failure_class = 'test_failure'))
                   )
                 ORDER BY r.id DESC LIMIT ?1",
            )
            .map_err(sqlite_error)?;
        let rows = stmt
            .query_map([GATE_CACHE_LIMIT], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        let mut found: Vec<Blocker> = Vec::new();
        for (gate, tree, session_id) in rows {
            let id = format!("gatecache:{gate}@{tree}");
            if found.iter().any(|blocker| blocker.id == id) {
                continue;
            }
            found.push(Blocker {
                clear: format!(
                    "{UNBLOCK} {id} --reason \"<why this verdict was not the code's fault>\""
                ),
                id,
                kind: BlockerKind::GateCache,
                scope: BlockerScope::Repo,
                cause: format!(
                    "gate {gate} has a cached failing verdict for tree {}; every run on that \
                     tree reuses it without executing the gate. Invalidate it only when the \
                     failure came from the host, not the code",
                    short(&tree)
                ),
                session_id,
                safe_to_clear_automatically: false,
            });
        }
        Ok(found)
    }

    fn pidfile_blockers(&self) -> Vec<Blocker> {
        let mut found = Vec::new();
        for pidfile in gate_pidfiles(self.main_root()) {
            if pidfile.pid.is_some_and(process_alive) {
                continue;
            }
            found.push(Blocker {
                id: format!("pidfile:{}-{}", pidfile.session_id, pidfile.gate),
                kind: BlockerKind::Pidfile,
                scope: BlockerScope::Repo,
                cause: format!(
                    "gate {} pidfile names {}, which is no longer running; cancellation would \
                     signal whatever process now holds that id",
                    pidfile.gate,
                    pidfile
                        .pid
                        .map_or_else(|| "no readable pid".to_string(), |pid| format!("pid {pid}")),
                ),
                session_id: Some(pidfile.session_id),
                clear: format!("{UNBLOCK} pidfile:{}-{}", pidfile.session_id, pidfile.gate),
                safe_to_clear_automatically: true,
            });
        }
        found
    }

    fn action_required_blockers(&self) -> Result<Vec<Blocker>, BrokerOpError> {
        let mut found = Vec::new();
        for session in self.store_ref().live_sessions()? {
            if session.status.is_closed() {
                continue;
            }
            let notice =
                Path::new(&session.worktree_path).join(crate::merge::ACTION_REQUIRED_RELPATH);
            if !notice.is_file() {
                continue;
            }
            found.push(Blocker {
                id: format!("action:{}", session.id),
                kind: BlockerKind::ActionRequired,
                scope: BlockerScope::Repo,
                cause: format!(
                    "session {}'s submission conflicted; {} names the files, the blocking \
                     session, and the rebase steps",
                    session.id,
                    notice.display()
                ),
                session_id: Some(session.id),
                clear: format!("aethyme broker submit --session {}", session.id),
                safe_to_clear_automatically: false,
            });
        }
        Ok(found)
    }

    // ── clearing ────────────────────────────────────────────────────

    fn unblock_operation(
        &mut self,
        request: &UnblockRequest,
        id: i64,
    ) -> Result<UnblockOutcome, BrokerOpError> {
        let operation = self
            .store()
            .coordinated_operation(id)?
            .ok_or(crate::BrokerError::CoordinatedOperationNotFound(id))?;
        if operation.status != OperationStatus::OutcomeUnknown {
            return Ok(refuse(
                request,
                BlockerKind::Operation,
                format!(
                    "operation {id} is {}, not outcome_unknown; it blocks nothing",
                    operation.status.as_str()
                ),
                Vec::new(),
                false,
            ));
        }
        let Some((succeeded, reason)) = outcome_and_reason(request) else {
            return Ok(needs_outcome(request, BlockerKind::Operation));
        };
        let report = self.reconcile_coordinated_operation(id, succeeded, &reason)?;
        Ok(UnblockOutcome::Cleared(UnblockReport {
            id: request.id.clone(),
            kind: BlockerKind::Operation,
            cleared: true,
            action: format!(
                "operation {} reconciled as {}{}",
                report.operation.id,
                report.operation.status.as_str(),
                report
                    .operation
                    .host_operation_id
                    .as_deref()
                    .map(|hex| format!(" together with host operation {hex}"))
                    .unwrap_or_default()
            ),
        }))
    }

    fn unblock_host_operation(
        &mut self,
        request: &UnblockRequest,
        hex: &str,
    ) -> Result<UnblockOutcome, BrokerOpError> {
        let database = self.host_operation_database_path()?;
        let operation = crate::host_operation(&database, hex)?
            .ok_or_else(|| crate::HostOperationError::NotFound(hex.into()))?;
        if !matches!(
            operation.status,
            OperationStatus::Prepared | OperationStatus::Running | OperationStatus::OutcomeUnknown
        ) {
            return Ok(refuse(
                request,
                BlockerKind::HostOperation,
                format!(
                    "host operation {hex} is {}; it blocks nothing",
                    operation.status.as_str()
                ),
                Vec::new(),
                false,
            ));
        }
        if operation.status == OperationStatus::Running
            && process_alive(i64::from(operation.holder_pid))
        {
            return Ok(refuse(
                request,
                BlockerKind::HostOperation,
                format!(
                    "host operation {hex} is running under live pid {}; it is in progress, not \
                     blocked",
                    operation.holder_pid
                ),
                Vec::new(),
                false,
            ));
        }
        let Some((succeeded, reason)) = outcome_and_reason(request) else {
            return Ok(needs_outcome(request, BlockerKind::HostOperation));
        };
        // A repository row that journals this host operation is the richer
        // audit trail, and reconciling it reconciles the host row too.
        let linked = self
            .store()
            .coordinated_operations()?
            .into_iter()
            .find(|row| row.host_operation_id.as_deref() == Some(hex));
        if let Some(row) = linked {
            let report = self.reconcile_coordinated_operation(row.id, succeeded, &reason)?;
            return Ok(UnblockOutcome::Cleared(UnblockReport {
                id: request.id.clone(),
                kind: BlockerKind::HostOperation,
                cleared: true,
                action: format!(
                    "host operation {hex} reconciled through operation {} as {}",
                    report.operation.id,
                    report.operation.status.as_str()
                ),
            }));
        }
        let reconciled = crate::reconcile_host_operation(&database, hex, succeeded)?;
        self.record_cleared(
            request,
            BlockerKind::HostOperation,
            serde_json::json!({
                "remote_key": reconciled.remote_key,
                "outcome": reconciled.status.as_str(),
            }),
        )?;
        Ok(UnblockOutcome::Cleared(UnblockReport {
            id: request.id.clone(),
            kind: BlockerKind::HostOperation,
            cleared: true,
            action: format!(
                "host operation {hex} for {} reconciled as {}",
                reconciled.remote_key,
                reconciled.status.as_str()
            ),
        }))
    }

    fn unblock_resource_lease(
        &mut self,
        request: &UnblockRequest,
        lease_id: &str,
    ) -> Result<UnblockOutcome, BrokerOpError> {
        let resource_error =
            |error: crate::HostResourceError| BrokerOpError::InvalidCoordinatedOperation {
                reason: error.to_string(),
            };
        let blocker = self
            .resource_lease_blockers()?
            .into_iter()
            .find(|blocker| blocker.id == request.id);
        let mut coordinator =
            crate::HostResourceCoordinator::open_default().map_err(resource_error)?;
        let lease = coordinator
            .list(false)
            .map_err(resource_error)?
            .into_iter()
            .find(|lease| lease.lease_id == lease_id)
            .ok_or_else(|| BrokerOpError::InvalidCoordinatedOperation {
                reason: format!("resource lease {lease_id} is not held; nothing to clear"),
            })?;
        if let Some(pid) = lease
            .holder_pid
            .filter(|pid| process_alive(i64::from(*pid)))
        {
            return Ok(refuse(
                request,
                BlockerKind::ResourceLease,
                format!("resource lease {lease_id} is held by live pid {pid}; it is in use"),
                Vec::new(),
                false,
            ));
        }
        if lease.state != crate::HostLeaseState::Quarantined {
            return Ok(refuse(
                request,
                BlockerKind::ResourceLease,
                format!(
                    "resource lease {lease_id} is {}, not quarantined",
                    lease.state.as_str()
                ),
                Vec::new(),
                false,
            ));
        }
        let generation =
            match request.confirm.as_deref() {
                Some(confirm) => confirm.parse::<u64>().map_err(|_| {
                    BrokerOpError::InvalidCoordinatedOperation {
                        reason: "--confirm must be the lease's full numeric generation".into(),
                    }
                })?,
                None if blocker
                    .as_ref()
                    .is_some_and(|b| b.safe_to_clear_automatically) =>
                {
                    lease.generation
                }
                None => {
                    return Ok(refuse(
                        request,
                        BlockerKind::ResourceLease,
                        format!(
                            "resource lease {lease_id} may still have residue on the host \
                         (containers, volumes, ports) and its owning session is not closed; \
                         inspect its allocations, then confirm generation {}",
                            lease.generation
                        ),
                        vec![format!("--confirm {}", lease.generation)],
                        false,
                    ));
                }
            };
        let released = coordinator
            .reconcile(lease_id, generation)
            .map_err(resource_error)?;
        self.record_cleared(
            request,
            BlockerKind::ResourceLease,
            serde_json::json!({ "generation": released.generation }),
        )?;
        Ok(UnblockOutcome::Cleared(UnblockReport {
            id: request.id.clone(),
            kind: BlockerKind::ResourceLease,
            cleared: true,
            action: format!(
                "resource lease {lease_id} generation {} {}",
                released.generation,
                released.state.as_str()
            ),
        }))
    }

    fn unblock_path_lease(
        &mut self,
        request: &UnblockRequest,
        lease_id: i64,
    ) -> Result<UnblockOutcome, BrokerOpError> {
        let lease = self
            .store()
            .active_leases()?
            .into_iter()
            .find(|lease| lease.id == lease_id)
            .ok_or_else(|| BrokerOpError::InvalidCoordinatedOperation {
                reason: format!("lease {lease_id} is not active; nothing to clear"),
            })?;
        let session = self.store().session(lease.session_id)?;
        if session.status != SessionStatus::Stale || Path::new(&session.worktree_path).exists() {
            return Ok(refuse(
                request,
                BlockerKind::PathLease,
                format!(
                    "lease {lease_id} on {} belongs to session {} ({}) whose worktree may hold \
                     uncommitted work; release it from that session or close it: aethyme broker \
                     close --session {}",
                    lease.path,
                    session.id,
                    session.status.as_str(),
                    session.id
                ),
                Vec::new(),
                false,
            ));
        }
        self.store().release_lease(session.id, &lease.path)?;
        Ok(UnblockOutcome::Cleared(UnblockReport {
            id: request.id.clone(),
            kind: BlockerKind::PathLease,
            cleared: true,
            action: format!(
                "lease {lease_id} on {} released from session {}",
                lease.path, session.id
            ),
        }))
    }

    fn unblock_gate_cache(
        &mut self,
        request: &UnblockRequest,
        gate: &str,
        tree: &str,
    ) -> Result<UnblockOutcome, BrokerOpError> {
        let Some(reason) = request
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
        else {
            return Ok(refuse(
                request,
                BlockerKind::GateCache,
                "invalidating a verdict is an audited operator action; say why the verdict was \
                 not the code's fault"
                    .into(),
                vec!["--reason <text>".into()],
                false,
            ));
        };
        if let Some(running) = gate_pidfiles(self.main_root()).into_iter().find(|pidfile| {
            pidfile.gate == gate
                && pidfile.tree.as_deref() == Some(tree)
                && pidfile.pid.is_some_and(process_alive)
        }) {
            return Ok(refuse(
                request,
                BlockerKind::GateCache,
                format!(
                    "session {} is gating {gate} on this tree right now; wait for it to finish",
                    running.session_id
                ),
                Vec::new(),
                false,
            ));
        }
        let removed = delete_failing_verdicts(&crate::broker_db_path(self.main_root()), gate, tree)
            .map_err(sqlite_error)?;
        if removed.is_empty() {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "no cached failing verdict for gate {gate} on tree {tree}; nothing to clear"
                ),
            });
        }
        self.record_cleared(
            request,
            BlockerKind::GateCache,
            serde_json::json!({
                "gate_name": gate,
                "tree_hash": tree,
                "removed_gate_result_ids": removed,
                "operator_reason": reason,
            }),
        )?;
        Ok(UnblockOutcome::Cleared(UnblockReport {
            id: request.id.clone(),
            kind: BlockerKind::GateCache,
            cleared: true,
            action: format!(
                "removed {} cached failing {} for gate {gate} on tree {}; the next run executes \
                 the gate",
                removed.len(),
                if removed.len() == 1 {
                    "verdict"
                } else {
                    "verdicts"
                },
                short(tree)
            ),
        }))
    }

    fn unblock_pidfile(
        &mut self,
        request: &UnblockRequest,
        session_id: i64,
        gate: &str,
    ) -> Result<UnblockOutcome, BrokerOpError> {
        let pidfile = gate_pidfiles(self.main_root())
            .into_iter()
            .find(|pidfile| pidfile.session_id == session_id && pidfile.gate == gate)
            .ok_or_else(|| BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "no pidfile for session {session_id} gate {gate}; nothing to clear"
                ),
            })?;
        if let Some(pid) = pidfile.pid.filter(|pid| process_alive(*pid)) {
            return Ok(refuse(
                request,
                BlockerKind::Pidfile,
                format!("gate {gate} is still running as pid {pid}; the pidfile is live"),
                Vec::new(),
                false,
            ));
        }
        std::fs::remove_file(&pidfile.path).map_err(|source| BrokerOpError::OperationIo {
            path: pidfile.path.clone(),
            source,
        })?;
        self.record_cleared(
            request,
            BlockerKind::Pidfile,
            serde_json::json!({ "gate_name": gate, "pid": pidfile.pid }),
        )?;
        Ok(UnblockOutcome::Cleared(UnblockReport {
            id: request.id.clone(),
            kind: BlockerKind::Pidfile,
            cleared: true,
            action: format!("removed stale pidfile {}", pidfile.path.display()),
        }))
    }

    // ── helpers ─────────────────────────────────────────────────────

    fn record_cleared(
        &mut self,
        request: &UnblockRequest,
        kind: BlockerKind,
        detail: serde_json::Value,
    ) -> Result<(), BrokerOpError> {
        let payload = serde_json::json!({
            "id": request.id,
            "kind": kind.as_str(),
            "reason": request.reason,
            "detail": detail,
        });
        self.store()
            .append_event(BLOCKER_CLEARED, None, Some(&payload.to_string()))?;
        Ok(())
    }

    fn action_required_path(&mut self, session_id: i64) -> Result<PathBuf, BrokerOpError> {
        let session = self.store().session(session_id)?;
        Ok(Path::new(&session.worktree_path).join(crate::merge::ACTION_REQUIRED_RELPATH))
    }

    /// Every identity under which this repository's remote writes are
    /// journaled on the host ledger.
    fn repository_coordination_keys(&self) -> Vec<String> {
        let mut keys = vec![format!("local:{}", self.main_root().display())];
        if let Ok(repo) = GitRepo::discover(self.main_root())
            && let Ok(target) = repo.resolve_remote_target("origin", None)
        {
            keys.push(target.coordination_key);
        }
        if let Ok(pending) = self.store_ref().pending_coordinated_operations() {
            keys.extend(pending.into_iter().map(|operation| operation.repository));
        }
        keys.sort();
        keys.dedup();
        keys
    }

    fn origin_fingerprint(&self) -> String {
        GitRepo::discover(self.main_root())
            .map(|repo| crate::gates::git_origin_fingerprint(&repo))
            .unwrap_or_default()
    }

    /// Resource leases name their worktree by digest; map it back to the
    /// session that owned the worktree.
    fn sessions_by_worktree_fingerprint(
        &self,
    ) -> Result<Vec<(String, crate::Session)>, BrokerOpError> {
        let mut sessions = self.store_ref().live_sessions()?;
        sessions.extend(self.store_ref().cleaned_sessions()?);
        let mut mapped = Vec::new();
        for session in sessions {
            mapped.push((sha256_text(&session.worktree_path), session.clone()));
            if let Ok(canonical) = std::fs::canonicalize(&session.worktree_path) {
                mapped.push((sha256_text(&canonical.to_string_lossy()), session));
            }
        }
        Ok(mapped)
    }
}

/// One status advice entry naming the blockers an operator can clear with
/// `broker unblock`. Cached failing verdicts are left out on purpose: they are
/// the code's verdict until someone shows the host caused them, and advice
/// that points at invalidating them would teach agents to retry red tests.
/// Conflict notices already have their own submit advice.
pub(crate) fn status_advice(blockers: &[Blocker]) -> Option<crate::StatusAdvice> {
    let actionable: Vec<&Blocker> = blockers
        .iter()
        .filter(|blocker| {
            !matches!(
                blocker.kind,
                BlockerKind::GateCache | BlockerKind::ActionRequired
            )
        })
        .collect();
    if actionable.is_empty() {
        return None;
    }
    let severity = if actionable.iter().any(|blocker| {
        matches!(
            blocker.kind,
            BlockerKind::Operation | BlockerKind::HostOperation
        )
    }) {
        crate::StatusAdviceSeverity::Blocked
    } else if actionable
        .iter()
        .any(|blocker| !blocker.safe_to_clear_automatically)
    {
        crate::StatusAdviceSeverity::Warning
    } else {
        crate::StatusAdviceSeverity::Notice
    };
    let count = actionable.len();
    Some(crate::StatusAdvice {
        id: "blockers.present",
        severity,
        reason: "coordination state is blocking work; each blocker names the one command that clears it",
        summary: format!(
            "{count} {} across broker stores; `aethyme broker blockers` lists them with causes",
            if count == 1 { "blocker" } else { "blockers" }
        ),
        session_id: None,
        queue_entry_id: None,
        evidence: actionable
            .iter()
            .take(5)
            .map(|blocker| format!("{}: {}", blocker.id, blocker.cause))
            .collect(),
        commands: actionable
            .iter()
            .take(5)
            .map(|blocker| blocker.clear.clone())
            .collect(),
    })
}

const UNBLOCK: &str = "aethyme broker unblock";
const OUTCOME_FLAGS: &str = "--outcome <succeeded|failed> --reason \"<what the remote shows>\"";

fn outcome_and_reason(request: &UnblockRequest) -> Option<(bool, String)> {
    let reason = request
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())?;
    Some((request.outcome?, reason.to_string()))
}

fn needs_outcome(request: &UnblockRequest, kind: BlockerKind) -> UnblockOutcome {
    refuse(
        request,
        kind,
        "the remote write may or may not have happened; only an operator who inspected the \
         remote can say which, so the broker never guesses"
            .into(),
        vec![
            "--outcome <succeeded|failed>".into(),
            "--reason <what the remote shows>".into(),
        ],
        true,
    )
}

fn refuse(
    request: &UnblockRequest,
    kind: BlockerKind,
    reason: String,
    required_flags: Vec<String>,
    needs_outcome: bool,
) -> UnblockOutcome {
    UnblockOutcome::Refused(UnblockRefusal {
        id: request.id.clone(),
        kind,
        cleared: false,
        reason,
        required_flags,
        needs_outcome,
    })
}

fn source_error(source: &'static str, error: impl std::fmt::Display) -> BlockerSourceError {
    BlockerSourceError {
        source,
        error: error.to_string(),
    }
}

fn sqlite_error(error: rusqlite::Error) -> BrokerOpError {
    BrokerOpError::from(crate::BrokerError::from(error))
}

fn host_read_error(error: rusqlite::Error) -> BrokerOpError {
    BrokerOpError::from(crate::HostOperationError::from(error))
}

fn sha256_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn short(tree: &str) -> &str {
    &tree[..12.min(tree.len())]
}

/// Whether `pid` names a process that still exists. `EPERM` means it exists
/// under another user, which is still alive for every purpose here.
pub(crate) fn process_alive(pid: i64) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

struct HostOperationRow {
    operation_id: String,
    remote_key: String,
    status: String,
    holder_pid: i64,
}

/// Unresolved rows in the host ledger, read without creating or migrating it.
fn unresolved_host_operations(database: &Path) -> Result<Vec<HostOperationRow>, rusqlite::Error> {
    if !database.exists() {
        return Ok(Vec::new());
    }
    let conn = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'host_operations'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT operation_id, remote_key, status, holder_pid FROM host_operations
         WHERE status IN ('running', 'outcome_unknown')
         ORDER BY created_at, operation_id",
    )?;
    stmt.query_map([], |row| {
        Ok(HostOperationRow {
            operation_id: row.get(0)?,
            remote_key: row.get(1)?,
            status: row.get(2)?,
            holder_pid: row.get(3)?,
        })
    })?
    .collect()
}

/// Delete the conclusive failing verdicts for one gate on one tree, and
/// nothing else: passes, cancellations, and infra-classified rows never
/// satisfied the cache and are history, not verdicts.
fn delete_failing_verdicts(
    database: &Path,
    gate: &str,
    tree: &str,
) -> Result<Vec<i64>, rusqlite::Error> {
    let mut conn = Connection::open(database)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let ids = {
        let mut stmt = tx.prepare(
            "SELECT id FROM gate_results
             WHERE gate_name = ?1 AND tree_hash = ?2
               AND status = 'fail' AND failure_class = 'test_failure'
             ORDER BY id",
        )?;
        stmt.query_map(params![gate, tree], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    for id in &ids {
        tx.execute("DELETE FROM gate_results WHERE id = ?1", [id])?;
    }
    tx.commit()?;
    Ok(ids)
}

struct GatePidfile {
    path: PathBuf,
    session_id: i64,
    gate: String,
    pid: Option<i64>,
    tree: Option<String>,
}

/// Pidfiles under `.aethyme/run/gates`, named `<session>-<gate>.pid` and
/// holding `<pgid> <tree>`.
fn gate_pidfiles(main_root: &Path) -> Vec<GatePidfile> {
    let Ok(entries) = std::fs::read_dir(main_root.join(".aethyme/run/gates")) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".pid") else {
            continue;
        };
        let Some((session, gate)) = stem.split_once('-') else {
            continue;
        };
        let Ok(session_id) = session.parse::<i64>() else {
            continue;
        };
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        // The gate runner owns the format; a leader PID is recorded since
        // v0.8.x, and older files name only the group, whose leader it is.
        let record = crate::gates::GatePidRecord::parse(&content);
        let pid = record
            .as_ref()
            .map(|record| i64::from(record.pid.unwrap_or(record.pgid)));
        let tree = record.map(|record| record.tree);
        found.push(GatePidfile {
            path: entry.path(),
            session_id,
            gate: gate.to_string(),
            pid,
            tree,
        });
    }
    found.sort_by(|a, b| (a.session_id, &a.gate).cmp(&(b.session_id, &b.gate)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_blocker_id_round_trips_through_one_namespace() {
        let hex = "2fe49869fe8c8bed0b3efa3e80482989";
        assert_eq!(BlockerRef::parse("op:7"), Ok(BlockerRef::Operation(7)));
        assert_eq!(
            BlockerRef::parse(&format!("hostop:{hex}")),
            Ok(BlockerRef::HostOperation(hex.into()))
        );
        assert_eq!(
            BlockerRef::parse("gatecache:rust-test@abc123"),
            Ok(BlockerRef::GateCache {
                gate: "rust-test".into(),
                tree: "abc123".into()
            })
        );
        assert_eq!(
            BlockerRef::parse("pidfile:12-rust-test"),
            Ok(BlockerRef::Pidfile {
                session_id: 12,
                gate: "rust-test".into()
            })
        );
        assert_eq!(BlockerRef::parse("lease:3"), Ok(BlockerRef::PathLease(3)));
        assert_eq!(
            BlockerRef::parse("resource:abc"),
            Ok(BlockerRef::ResourceLease("abc".into()))
        );
        assert_eq!(
            BlockerRef::parse("action:4"),
            Ok(BlockerRef::ActionRequired(4))
        );
        for bad in ["op:0", "op:x", "hostop:12", "gatecache:x", "nope:1", "7"] {
            assert!(BlockerRef::parse(bad).is_err(), "{bad} must not parse");
        }
    }

    #[test]
    fn a_process_that_exited_is_not_alive() {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = i64::from(child.id());
        child.wait().unwrap();
        assert!(!process_alive(pid));
        assert!(process_alive(i64::from(std::process::id())));
    }
}
