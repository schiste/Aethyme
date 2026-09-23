//! Per-user, host-scoped coordination for scarce validation resources.
//!
//! This is intentionally separate from repository path leases: it must
//! coordinate independent clones, and it must never imply source ownership.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const HOST_RESOURCE_SCHEMA_VERSION: u32 = 1;
pub const HOST_RESOURCE_REQUEST_SCHEMA_VERSION: u32 = 1;
const MIN_TTL_SECONDS: u64 = 15;
const MAX_TTL_SECONDS: u64 = 86_400;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
INSERT OR IGNORE INTO meta VALUES ('schema_version', 1);
INSERT OR IGNORE INTO meta VALUES ('generation', 0);

CREATE TABLE IF NOT EXISTS resource_leases (
 lease_id TEXT PRIMARY KEY,
 request_id TEXT NOT NULL UNIQUE,
 request_digest TEXT NOT NULL,
 repository TEXT NOT NULL,
 worktree_fingerprint TEXT NOT NULL,
 run_id TEXT NOT NULL,
 generation INTEGER NOT NULL UNIQUE,
 ownership_token TEXT NOT NULL,
 state TEXT NOT NULL CHECK (state IN ('active','quarantined','released')),
 holder_pid INTEGER,
 created_at INTEGER NOT NULL,
 expires_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,
 released_at INTEGER
);
CREATE TABLE IF NOT EXISTS resource_allocations (
 lease_id TEXT NOT NULL REFERENCES resource_leases(lease_id),
 resource_key TEXT NOT NULL,
 kind TEXT NOT NULL CHECK (kind IN ('namespace','tcp_port','capacity','exclusive_key')),
 value TEXT NOT NULL,
 units INTEGER,
 capacity_limit INTEGER,
 PRIMARY KEY (lease_id, resource_key)
);
CREATE INDEX IF NOT EXISTS resource_leases_by_state ON resource_leases(state, expires_at);
CREATE INDEX IF NOT EXISTS resource_allocations_by_value
 ON resource_allocations(kind, value, lease_id);
CREATE TABLE IF NOT EXISTS resource_reap_observations (
 lease_id TEXT PRIMARY KEY REFERENCES resource_leases(lease_id),
 generation INTEGER NOT NULL,
 reaped_at INTEGER NOT NULL
);
"#;

#[derive(Debug, thiserror::Error)]
pub enum HostResourceError {
    #[error(
        "host resource state at {}",
        crate::host_state::describe_host_state_io(path, source)
    )]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("host resource database: {}", crate::host_state::describe_host_state_sqlite(.0))]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid host resource request: {0}")]
    InvalidRequest(String),
    #[error("request {0:?} was already used with different contents")]
    IdempotencyMismatch(String),
    #[error("request {request_id:?} is {state}; use a new request id")]
    RequestNotActive { request_id: String, state: String },
    #[error("host resource bundle unavailable: {message}")]
    Conflict {
        code: String,
        message: String,
        conflicts: Vec<HostResourceConflict>,
    },
    #[error("no host resource lease {0:?}")]
    LeaseNotFound(String),
    #[error("ownership credentials do not match host resource lease {0:?}")]
    OwnershipMismatch(String),
    #[error("lease {lease_id:?} generation is {actual}, not {confirmed}")]
    GenerationMismatch {
        lease_id: String,
        confirmed: u64,
        actual: u64,
    },
    #[error("lease {lease_id:?} is {state}, not quarantined")]
    NotQuarantined { lease_id: String, state: String },
    #[error("cannot find per-user state directory; set AETHYME_HOST_STATE_DIR")]
    StateDirectoryUnavailable,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceRequest {
    pub schema_version: u32,
    pub request_id: String,
    pub repository: String,
    /// Opaque digest. Absolute worktree paths are never persisted.
    pub worktree_fingerprint: String,
    pub run_id: String,
    pub ttl_seconds: u64,
    #[serde(default)]
    pub holder_pid: Option<u32>,
    pub resources: Vec<HostResourceRequirement>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceRequirement {
    pub key: String,
    #[serde(flatten)]
    pub resource: HostResourceKind,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostResourceKind {
    Namespace {
        prefix: String,
    },
    TcpPort {
        start: u16,
        end: u16,
    },
    Capacity {
        pool: String,
        units: u32,
        limit: u32,
    },
    ExclusiveKey {
        name: String,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostLeaseState {
    Active,
    Quarantined,
    Released,
}

impl HostLeaseState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Quarantined => "quarantined",
            Self::Released => "released",
        }
    }
    fn parse(value: &str) -> Result<Self, HostResourceError> {
        match value {
            "active" => Ok(Self::Active),
            "quarantined" => Ok(Self::Quarantined),
            "released" => Ok(Self::Released),
            _ => Err(HostResourceError::InvalidRequest(format!(
                "invalid stored state {value:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceAllocation {
    pub key: String,
    pub kind: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity_limit: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceLease {
    pub lease_id: String,
    pub request_id: String,
    pub repository: String,
    pub worktree_fingerprint: String,
    pub run_id: String,
    pub generation: u64,
    pub state: HostLeaseState,
    pub holder_pid: Option<u32>,
    pub created_at: i64,
    pub expires_at: i64,
    pub released_at: Option<i64>,
    pub allocations: Vec<HostResourceAllocation>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceGrant {
    pub lease: HostResourceLease,
    /// Capability for renew/release; inventory and reports never include it.
    pub ownership_token: String,
}

impl HostResourceGrant {
    pub fn environment(&self) -> BTreeMap<String, String> {
        let mut values = BTreeMap::from([
            (
                "AETHYME_RESOURCE_LEASE_ID".into(),
                self.lease.lease_id.clone(),
            ),
            (
                "AETHYME_RESOURCE_GENERATION".into(),
                self.lease.generation.to_string(),
            ),
        ]);
        for allocation in &self.lease.allocations {
            values.insert(
                resource_environment_key(&allocation.key),
                allocation.value.clone(),
            );
        }
        values
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceConflict {
    /// Stable automation code. Capacity-policy disagreement is distinct from
    /// ordinary contention because waiting cannot resolve configuration skew.
    pub code: String,
    pub resource_key: String,
    pub kind: String,
    pub requested: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owning_lease: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourcePlan {
    pub schema_version: u32,
    pub request_id: String,
    pub available: bool,
    pub proposed: Vec<HostResourceAllocation>,
    pub conflicts: Vec<HostResourceConflict>,
    /// Always true: only acquire reserves a resource.
    pub advisory: bool,
}

/// A read-only diagnosis of why a resource request is or is not runnable.
/// Unlike [`HostResourcePlan`], this joins every conflict to the lease and
/// holder information an operator needs to decide whether waiting or
/// generation-fenced reconciliation is appropriate.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceExplanation {
    pub schema_version: u32,
    pub request_id: String,
    pub request_digest: String,
    pub available: bool,
    pub proposed: Vec<HostResourceAllocation>,
    pub blockers: Vec<HostResourceBlocker>,
    pub wait: HostResourceWaitAdvice,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceBlocker {
    pub conflict: HostResourceConflict,
    /// Full public lease records, never ownership tokens. There can be more
    /// than one holder for a capacity pool or a port range.
    pub leases: Vec<HostResourceLease>,
    pub holders: Vec<HostResourceHolder>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_bindable: Option<bool>,
    pub recovery: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceHolder {
    pub lease_id: String,
    pub generation: u64,
    pub run_id: String,
    pub repository: String,
    pub worktree_fingerprint: String,
    pub state: HostLeaseState,
    pub holder_pid: Option<u32>,
    pub process_alive: Option<bool>,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct HostResourceWaitAdvice {
    pub waitable: bool,
    pub reason: String,
    pub action: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostResourceRunReport {
    pub schema_version: u32,
    pub request_id: String,
    pub lease_id: String,
    pub generation: u64,
    pub waited_ms: u128,
    pub child_exit_code: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_signal: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_exit_code: Option<u8>,
    pub authority_lost: bool,
    pub final_lease_state: HostLeaseState,
}

/// Public, non-secret evidence from an independent dead-holder sweep.
///
/// Capacity is safe to reclaim once the owning process is provably gone. Named
/// allocations remain quarantined because they may still have residue that
/// requires an operator's cleanup review.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostResourceReapReport {
    pub schema_version: u32,
    pub dead_holders_seen: u32,
    pub reclaimed_capacity_units: u32,
    pub released_leases: u32,
    pub retained_quarantined_leases: u32,
    pub leases: Vec<HostResourceReapLease>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostResourceReapLease {
    pub lease_id: String,
    pub generation: u64,
    pub holder_pid: u32,
    pub capacity_units: u32,
    pub state: HostLeaseState,
}

#[derive(Debug, thiserror::Error)]
pub enum HostResourceRunError {
    #[error(transparent)]
    Resource(#[from] HostResourceError),
    #[error("cannot prepare supervised command environment: {0}")]
    Environment(#[source] std::io::Error),
    #[error("cannot start supervised command {program:?}: {source}")]
    Spawn {
        program: String,
        source: std::io::Error,
    },
    #[error("cannot supervise process signals: {0}")]
    Signals(std::io::Error),
}

pub struct HostResourceCoordinator {
    conn: Connection,
    path: PathBuf,
}

impl HostResourceCoordinator {
    pub fn open_default() -> Result<Self, HostResourceError> {
        Self::open(&default_host_resource_db_path()?)
    }

    pub fn open(path: &Path) -> Result<Self, HostResourceError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| HostResourceError::Io {
                path: parent.into(),
                source,
            })?;
            crate::host_state::protect_host_state_path(parent, true).map_err(|source| {
                HostResourceError::Io {
                    path: parent.into(),
                    source,
                }
            })?;
        }
        let conn = Connection::open(path)?;
        crate::host_state::protect_host_state_path(path, false).map_err(|source| {
            HostResourceError::Io {
                path: path.into(),
                source,
            }
        })?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        validate_schema(&conn)?;
        Ok(Self {
            conn,
            path: path.into(),
        })
    }

    /// Open without touching durable state. A missing registry is represented
    /// by an initialized in-memory database, so first-use planning stays pure.
    pub fn open_read_only_default() -> Result<Self, HostResourceError> {
        Self::open_read_only(&default_host_resource_db_path()?)
    }

    pub fn open_read_only(path: &Path) -> Result<Self, HostResourceError> {
        if !path.exists() {
            let conn = Connection::open_in_memory()?;
            conn.execute_batch(SCHEMA)?;
            return Ok(Self {
                conn,
                path: path.into(),
            });
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        validate_schema(&conn)?;
        Ok(Self {
            conn,
            path: path.into(),
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.path
    }

    /// Read-only estimate. A later acquire may race and is authoritative.
    pub fn plan(
        &self,
        request: &HostResourceRequest,
    ) -> Result<HostResourcePlan, HostResourceError> {
        validate_request(request)?;
        let occupied = load_occupied(&self.conn)?;
        let (proposed, conflicts) = plan_allocations(request, &occupied)?;
        Ok(HostResourcePlan {
            schema_version: HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: request.request_id.clone(),
            available: conflicts.is_empty(),
            proposed,
            conflicts,
            advisory: true,
        })
    }

    /// Explain a plan using only read queries. The returned blocker records
    /// intentionally carry no ownership credential, and this method never
    /// reclaims, renews, or otherwise changes host state.
    pub fn explain(
        &self,
        request: &HostResourceRequest,
    ) -> Result<HostResourceExplanation, HostResourceError> {
        let plan = self.plan(request)?;
        let leases = self.list(false)?;
        let blockers = plan
            .conflicts
            .iter()
            .cloned()
            .map(|conflict| {
                let leases = leases_for_conflict(&conflict, &leases);
                let holders = leases.iter().map(host_resource_holder).collect();
                let os_bindable =
                    (conflict.kind == "tcp_port").then(|| any_port_bindable(&conflict.requested));
                let recovery = blocker_recovery(&conflict, &leases);
                HostResourceBlocker {
                    conflict,
                    leases,
                    holders,
                    os_bindable,
                    recovery,
                }
            })
            .collect::<Vec<_>>();
        let wait = wait_advice(&blockers);
        Ok(HostResourceExplanation {
            schema_version: HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: request.request_id.clone(),
            request_digest: request_digest(request)?,
            available: plan.available,
            proposed: plan.proposed,
            blockers,
            wait,
        })
    }

    /// Acquires the whole bundle in one immediate SQLite transaction.
    pub fn acquire(
        &mut self,
        request: &HostResourceRequest,
    ) -> Result<HostResourceGrant, HostResourceError> {
        validate_request(request)?;
        let digest = request_digest(request)?;
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let _ = quarantine_reclaimable(&tx, now)?;
        if let Some((existing_digest, state, token)) = tx.query_row(
            "SELECT request_digest,state,ownership_token FROM resource_leases WHERE request_id=?1",
            [&request.request_id],
            |row| Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,String>(2)?)),
        ).optional()? {
            if existing_digest != digest { return Err(HostResourceError::IdempotencyMismatch(request.request_id.clone())); }
            if state != "active" { return Err(HostResourceError::RequestNotActive { request_id: request.request_id.clone(), state }); }
            let lease = load_lease(&tx, "request_id", &request.request_id)?.ok_or_else(|| HostResourceError::LeaseNotFound(request.request_id.clone()))?;
            tx.commit()?;
            return Ok(HostResourceGrant { lease, ownership_token: token });
        }
        let occupied = load_occupied(&tx)?;
        let (allocations, conflicts) = plan_allocations(request, &occupied)?;
        if !conflicts.is_empty() {
            let code = aggregate_conflict_code(&conflicts).to_string();
            let message = conflicts
                .iter()
                .map(|c| format!("{}: {}", c.resource_key, c.reason))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(HostResourceError::Conflict {
                code,
                message,
                conflicts,
            });
        }
        let generation: i64 = tx.query_row(
            "UPDATE meta SET value=value+1 WHERE key='generation' RETURNING value",
            [],
            |row| row.get(0),
        )?;
        let lease_id = random_hex(&tx, 16)?;
        let ownership_token = random_hex(&tx, 32)?;
        let expires_at = now.saturating_add((request.ttl_seconds as i64).saturating_mul(1_000));
        tx.execute(
            "INSERT INTO resource_leases (lease_id,request_id,request_digest,repository,worktree_fingerprint,run_id,generation,ownership_token,state,holder_pid,created_at,expires_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'active',?9,?10,?11,?10)",
            params![lease_id,request.request_id,digest,request.repository,request.worktree_fingerprint,request.run_id,generation,ownership_token,request.holder_pid.map(i64::from),now,expires_at])?;
        for allocation in &allocations {
            tx.execute(
                "INSERT INTO resource_allocations VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    lease_id,
                    allocation.key,
                    allocation.kind,
                    allocation.value,
                    allocation.units.map(i64::from),
                    allocation.capacity_limit.map(i64::from)
                ],
            )?;
        }
        let lease = load_lease(&tx, "lease_id", &lease_id)?
            .ok_or_else(|| HostResourceError::LeaseNotFound(lease_id.clone()))?;
        tx.commit()?;
        Ok(HostResourceGrant {
            lease,
            ownership_token,
        })
    }

    /// Acquire a bundle, waiting only for ordinary contention. All other
    /// failures are returned immediately. The same request id is reused for
    /// every attempt so a successful acquire remains idempotent.
    pub fn acquire_with_wait<F>(
        &mut self,
        request: &HostResourceRequest,
        wait: std::time::Duration,
        mut on_conflict: F,
    ) -> Result<HostResourceGrant, HostResourceError>
    where
        F: FnMut(&str),
    {
        let deadline = std::time::Instant::now() + wait;
        loop {
            match self.acquire(request) {
                Ok(grant) => return Ok(grant),
                Err(HostResourceError::Conflict {
                    code: _,
                    message,
                    conflicts: _,
                }) if std::time::Instant::now() < deadline => {
                    on_conflict(&message);
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    std::thread::sleep(remaining.min(std::time::Duration::from_millis(250)));
                }
                Err(HostResourceError::Conflict {
                    code,
                    message,
                    conflicts,
                }) => {
                    return Err(HostResourceError::Conflict {
                        code,
                        message,
                        conflicts,
                    });
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn renew(
        &mut self,
        lease_id: &str,
        generation: u64,
        token: &str,
        ttl_seconds: u64,
    ) -> Result<HostResourceLease, HostResourceError> {
        validate_ttl(ttl_seconds)?;
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let _ = quarantine_reclaimable(&tx, now)?;
        verify_ownership(&tx, lease_id, generation, token, true)?;
        let expires = now.saturating_add((ttl_seconds as i64).saturating_mul(1_000));
        tx.execute(
            "UPDATE resource_leases SET expires_at=?2,updated_at=?3 WHERE lease_id=?1",
            params![lease_id, expires, now],
        )?;
        let lease = load_lease(&tx, "lease_id", lease_id)?
            .ok_or_else(|| HostResourceError::LeaseNotFound(lease_id.into()))?;
        tx.commit()?;
        Ok(lease)
    }

    pub fn release(
        &mut self,
        lease_id: &str,
        generation: u64,
        token: &str,
    ) -> Result<HostResourceLease, HostResourceError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let _ = quarantine_reclaimable(&tx, now)?;
        verify_ownership(&tx, lease_id, generation, token, false)?;
        tx.execute("UPDATE resource_leases SET state='released',released_at=?2,updated_at=?2 WHERE lease_id=?1 AND state!='released'", params![lease_id,now])?;
        let lease = load_lease(&tx, "lease_id", lease_id)?
            .ok_or_else(|| HostResourceError::LeaseNotFound(lease_id.into()))?;
        tx.commit()?;
        Ok(lease)
    }

    /// Conservatively fence a grant whose exact cleanup or renewal authority
    /// could not be proven. Quarantined allocations require generation-bound
    /// reconciliation and are never silently reused.
    pub fn quarantine(
        &mut self,
        lease_id: &str,
        generation: u64,
        token: &str,
    ) -> Result<HostResourceLease, HostResourceError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        verify_ownership(&tx, lease_id, generation, token, false)?;
        tx.execute(
            "UPDATE resource_leases SET state='quarantined',updated_at=?2
             WHERE lease_id=?1 AND state='active'",
            params![lease_id, now],
        )?;
        let lease = load_lease(&tx, "lease_id", lease_id)?
            .ok_or_else(|| HostResourceError::LeaseNotFound(lease_id.into()))?;
        tx.commit()?;
        Ok(lease)
    }

    /// Reclaim capacity held by quarantined leases whose holder process is
    /// provably gone. This is an explicit independent trigger: a scheduler or
    /// operator can run it without waiting for another acquisition to arrive.
    /// Named allocations stay quarantined until exact cleanup is reviewed.
    pub fn reap_dead_holders(&mut self) -> Result<HostResourceReapReport, HostResourceError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let report = quarantine_reclaimable(&tx, now)?;
        tx.commit()?;
        Ok(report)
    }

    /// Run an existing command under a complete host-resource lifecycle.
    /// Ownership credentials remain in this process; the child receives only
    /// public allocation values plus lease id and generation.
    pub fn run_supervised<F>(
        &mut self,
        request: &HostResourceRequest,
        wait: std::time::Duration,
        command: &[String],
        cleanup_command: Option<&str>,
        cwd: &Path,
        event: F,
    ) -> Result<HostResourceRunReport, HostResourceRunError>
    where
        F: FnMut(&str),
    {
        self.run_supervised_with_environment(
            request,
            wait,
            command,
            cleanup_command,
            cwd,
            |_grant| Ok(BTreeMap::new()),
            event,
        )
    }

    /// Run a command under a complete host-resource lifecycle, preparing
    /// supplemental environment values after acquisition has selected the
    /// actual allocations. This is the narrow hook consumers need when a
    /// child-facing value depends on the granted port or namespace.
    #[allow(clippy::too_many_arguments)]
    pub fn run_supervised_with_environment<F, P>(
        &mut self,
        request: &HostResourceRequest,
        wait: std::time::Duration,
        command: &[String],
        cleanup_command: Option<&str>,
        cwd: &Path,
        mut prepare_environment: P,
        mut event: F,
    ) -> Result<HostResourceRunReport, HostResourceRunError>
    where
        F: FnMut(&str),
        P: FnMut(&HostResourceGrant) -> Result<BTreeMap<String, String>, std::io::Error>,
    {
        if command.is_empty() {
            return Err(HostResourceRunError::Resource(
                HostResourceError::InvalidRequest("supervised command must not be empty".into()),
            ));
        }
        let acquire_started = std::time::Instant::now();
        let mut last_wait_notice = None;
        let mut grant = self.acquire_with_wait(request, wait, |message| {
            let emit = last_wait_notice.is_none_or(|last: std::time::Instant| {
                last.elapsed() >= std::time::Duration::from_secs(10)
            });
            if emit {
                event(&format!("waiting: {message}"));
                last_wait_notice = Some(std::time::Instant::now());
            }
        })?;
        let waited_ms = acquire_started.elapsed().as_millis();
        event(&format!(
            "granted lease {} generation {} after {}ms",
            grant.lease.lease_id, grant.lease.generation, waited_ms
        ));

        let environment = match prepare_environment(&grant) {
            Ok(environment) => environment,
            Err(source) => {
                let _ = self.quarantine(
                    &grant.lease.lease_id,
                    grant.lease.generation,
                    &grant.ownership_token,
                );
                return Err(HostResourceRunError::Environment(source));
            }
        };

        let child = spawn_resource_process(command, cwd, &grant, &environment).map_err(|source| {
            HostResourceRunError::Spawn {
                program: command[0].clone(),
                source,
            }
        });
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                let _ = self.quarantine(
                    &grant.lease.lease_id,
                    grant.lease.generation,
                    &grant.ownership_token,
                );
                return Err(error);
            }
        };
        let child_status = supervise_process(
            self,
            &mut grant,
            request.ttl_seconds,
            &mut child,
            &mut event,
        )?;
        let child_signal = exit_signal(&child_status);
        let child_exit_code = portable_exit_code(&child_status);

        let mut cleanup_exit_code = None;
        let mut lifecycle_safe = true;
        if let Some(cleanup) = cleanup_command {
            event("running exact cleanup command");
            let cleanup_argv = vec!["sh".to_string(), "-c".to_string(), cleanup.to_string()];
            let cleanup_child = spawn_resource_process(&cleanup_argv, cwd, &grant, &environment)
                .map_err(|source| HostResourceRunError::Spawn {
                    program: "sh".into(),
                    source,
                });
            let mut cleanup_child = match cleanup_child {
                Ok(child) => child,
                Err(error) => {
                    let _ = self.quarantine(
                        &grant.lease.lease_id,
                        grant.lease.generation,
                        &grant.ownership_token,
                    );
                    return Err(error);
                }
            };
            let cleanup_status = supervise_process(
                self,
                &mut grant,
                request.ttl_seconds,
                &mut cleanup_child,
                &mut event,
            )?;
            cleanup_exit_code = Some(portable_exit_code(&cleanup_status));
            lifecycle_safe = cleanup_status.success();
        }

        let authority_lost = grant.lease.state != HostLeaseState::Active;
        let final_lease = if lifecycle_safe && !authority_lost {
            self.release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )?
        } else {
            self.quarantine(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )?
        };
        event(match final_lease.state {
            HostLeaseState::Released => "released resource bundle",
            HostLeaseState::Quarantined => "quarantined resource bundle; reconciliation required",
            HostLeaseState::Active => "resource bundle remains active",
        });
        Ok(HostResourceRunReport {
            schema_version: HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: request.request_id.clone(),
            lease_id: final_lease.lease_id,
            generation: final_lease.generation,
            waited_ms,
            child_exit_code,
            child_signal,
            cleanup_exit_code,
            authority_lost,
            final_lease_state: final_lease.state,
        })
    }

    /// Reviewed crash recovery, fenced by the exact generation.
    pub fn reconcile(
        &mut self,
        lease_id: &str,
        confirmed: u64,
    ) -> Result<HostResourceLease, HostResourceError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let _ = quarantine_reclaimable(&tx, now)?;
        let (actual, state): (i64, String) = tx
            .query_row(
                "SELECT generation,state FROM resource_leases WHERE lease_id=?1",
                [lease_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| HostResourceError::LeaseNotFound(lease_id.into()))?;
        if actual as u64 != confirmed {
            return Err(HostResourceError::GenerationMismatch {
                lease_id: lease_id.into(),
                confirmed,
                actual: actual as u64,
            });
        }
        if state != "quarantined" {
            return Err(HostResourceError::NotQuarantined {
                lease_id: lease_id.into(),
                state,
            });
        }
        tx.execute("UPDATE resource_leases SET state='released',released_at=?2,updated_at=?2 WHERE lease_id=?1", params![lease_id,now])?;
        let lease = load_lease(&tx, "lease_id", lease_id)?
            .ok_or_else(|| HostResourceError::LeaseNotFound(lease_id.into()))?;
        tx.commit()?;
        Ok(lease)
    }

    /// Read-only inventory; expired active rows render as quarantined.
    pub fn list(
        &self,
        include_released: bool,
    ) -> Result<Vec<HostResourceLease>, HostResourceError> {
        let sql = if include_released {
            "SELECT lease_id FROM resource_leases ORDER BY generation"
        } else {
            "SELECT lease_id FROM resource_leases WHERE state!='released' ORDER BY generation"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut leases = Vec::new();
        for id in ids {
            if let Some(mut lease) = load_lease(&self.conn, "lease_id", &id)? {
                if lease.state == HostLeaseState::Active && lease.expires_at <= now_ms() {
                    lease.state = HostLeaseState::Quarantined;
                }
                leases.push(lease);
            }
        }
        Ok(leases)
    }
}

struct ResourceChild {
    child: std::process::Child,
    #[cfg(unix)]
    signals: BlockedSignals,
}

#[cfg(unix)]
struct BlockedSignals {
    watched: libc::sigset_t,
    previous: libc::sigset_t,
}

#[cfg(unix)]
impl BlockedSignals {
    fn new() -> std::io::Result<Self> {
        let mut watched = unsafe { std::mem::zeroed() };
        let mut previous = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut watched);
            libc::sigaddset(&mut watched, libc::SIGINT);
            libc::sigaddset(&mut watched, libc::SIGTERM);
            libc::sigaddset(&mut watched, libc::SIGHUP);
            if libc::pthread_sigmask(libc::SIG_BLOCK, &watched, &mut previous) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(Self { watched, previous })
    }

    fn pending(&self) -> Option<i32> {
        let mut pending = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigpending(&mut pending) } != 0 {
            return None;
        }
        let has_watched = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
            .into_iter()
            .any(|signal| unsafe { libc::sigismember(&pending, signal) } == 1);
        if !has_watched {
            return None;
        }
        let mut signal = 0;
        (unsafe { libc::sigwait(&self.watched, &mut signal) } == 0).then_some(signal)
    }
}

#[cfg(unix)]
impl Drop for BlockedSignals {
    fn drop(&mut self) {
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut());
        }
    }
}

fn spawn_resource_process(
    command: &[String],
    cwd: &Path,
    grant: &HostResourceGrant,
    environment: &BTreeMap<String, String>,
) -> std::io::Result<ResourceChild> {
    #[cfg(unix)]
    use std::os::unix::process::CommandExt as _;

    #[cfg(unix)]
    let signals = BlockedSignals::new()?;
    let mut process = std::process::Command::new(&command[0]);
    process
        .args(&command[1..])
        .current_dir(cwd)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    for (key, value) in environment {
        process.env(key, value);
    }
    // The coordinator owns allocation variables; supplemental values may add
    // a consumer contract but must not be able to make the child report a
    // different port, namespace, slot, or lease generation.
    for (key, value) in grant.environment() {
        process.env(key, value);
    }
    #[cfg(unix)]
    {
        let previous = signals.previous;
        process.process_group(0);
        unsafe {
            process.pre_exec(move || {
                if libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let child = process.spawn()?;
    Ok(ResourceChild {
        child,
        #[cfg(unix)]
        signals,
    })
}

fn supervise_process<F>(
    coordinator: &mut HostResourceCoordinator,
    grant: &mut HostResourceGrant,
    ttl_seconds: u64,
    process: &mut ResourceChild,
    event: &mut F,
) -> Result<std::process::ExitStatus, HostResourceRunError>
where
    F: FnMut(&str),
{
    let renewal_interval = std::time::Duration::from_secs((ttl_seconds / 3).max(1));
    let mut next_renewal = std::time::Instant::now() + renewal_interval;
    loop {
        if let Some(status) =
            process
                .child
                .try_wait()
                .map_err(|source| HostResourceRunError::Spawn {
                    program: "supervised child".into(),
                    source,
                })?
        {
            return Ok(status);
        }
        #[cfg(unix)]
        if let Some(signal) = process.signals.pending() {
            event(&format!(
                "forwarding signal {signal} to supervised process group"
            ));
            unsafe {
                libc::killpg(process.child.id() as i32, signal);
            }
        }
        if std::time::Instant::now() >= next_renewal {
            match coordinator.renew(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
                ttl_seconds,
            ) {
                Ok(lease) => {
                    grant.lease = lease;
                    next_renewal = std::time::Instant::now() + renewal_interval;
                }
                Err(error) if now_ms().saturating_add(1_000) < grant.lease.expires_at => {
                    event(&format!("resource renewal retry: {error}"));
                    next_renewal =
                        std::time::Instant::now() + std::time::Duration::from_millis(250);
                }
                Err(error) => {
                    event(&format!(
                        "resource authority lost; terminating process group: {error}"
                    ));
                    grant.lease.state = HostLeaseState::Quarantined;
                    #[cfg(unix)]
                    unsafe {
                        libc::killpg(process.child.id() as i32, libc::SIGTERM);
                    }
                    #[cfg(not(unix))]
                    let _ = process.child.kill();
                    return process
                        .child
                        .wait()
                        .map_err(|source| HostResourceRunError::Spawn {
                            program: "supervised child".into(),
                            source,
                        });
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn portable_exit_code(status: &std::process::ExitStatus) -> u8 {
    status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or_else(|| exit_signal(status).map_or(1, |signal| (128 + signal).min(255) as u8))
}

#[cfg(unix)]
fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

fn validate_schema(conn: &Connection) -> Result<(), HostResourceError> {
    let version: i64 = conn.query_row(
        "SELECT value FROM meta WHERE key='schema_version'",
        [],
        |row| row.get(0),
    )?;
    if version != i64::from(HOST_RESOURCE_SCHEMA_VERSION) {
        return Err(HostResourceError::InvalidRequest(format!(
            "unsupported host schema {version}"
        )));
    }
    Ok(())
}

pub fn default_host_resource_db_path() -> Result<PathBuf, HostResourceError> {
    crate::host_state::default_host_state_dir()
        .map(|directory| directory.join("host-resources.db"))
        .ok_or(HostResourceError::StateDirectoryUnavailable)
}

pub fn resource_environment_key(key: &str) -> String {
    let suffix = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("AETHYME_RESOURCE_{suffix}")
}

/// Validate a gate's declarative resource profile without opening host state.
pub fn validate_host_resource_requirements(
    resources: &[HostResourceRequirement],
    ttl_seconds: u64,
) -> Result<(), HostResourceError> {
    if resources.is_empty() {
        validate_ttl(ttl_seconds)?;
        return Ok(());
    }
    validate_request(&HostResourceRequest {
        schema_version: HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
        request_id: "gate-validation".into(),
        repository: "gate-validation".into(),
        worktree_fingerprint: "gate-validation".into(),
        run_id: "gate-validation".into(),
        ttl_seconds,
        holder_pid: None,
        resources: resources.to_vec(),
    })
}

fn validate_request(request: &HostResourceRequest) -> Result<(), HostResourceError> {
    if request.schema_version != HOST_RESOURCE_REQUEST_SCHEMA_VERSION {
        return Err(HostResourceError::InvalidRequest(format!(
            "schema_version must be {HOST_RESOURCE_REQUEST_SCHEMA_VERSION}"
        )));
    }
    for (field, value) in [
        ("request_id", &request.request_id),
        ("repository", &request.repository),
        ("worktree_fingerprint", &request.worktree_fingerprint),
        ("run_id", &request.run_id),
    ] {
        validate_identifier(field, value)?;
    }
    validate_ttl(request.ttl_seconds)?;
    if request.resources.is_empty() {
        return Err(HostResourceError::InvalidRequest(
            "resources must not be empty".into(),
        ));
    }
    let mut keys = BTreeSet::new();
    let mut env_keys = BTreeSet::new();
    for requirement in &request.resources {
        validate_identifier("resource key", &requirement.key)?;
        if !keys.insert(requirement.key.clone()) {
            return Err(HostResourceError::InvalidRequest(format!(
                "duplicate resource key {:?}",
                requirement.key
            )));
        }
        if !env_keys.insert(resource_environment_key(&requirement.key)) {
            return Err(HostResourceError::InvalidRequest(
                "resource keys collide after environment normalization".into(),
            ));
        }
        match &requirement.resource {
            HostResourceKind::Namespace { prefix } => {
                validate_identifier("namespace prefix", prefix)?
            }
            HostResourceKind::TcpPort { start, end } if *start == 0 || start > end => {
                return Err(HostResourceError::InvalidRequest(
                    "tcp_port needs a non-zero inclusive start..end range".into(),
                ));
            }
            HostResourceKind::TcpPort { .. } => {}
            HostResourceKind::Capacity { pool, units, limit } => {
                validate_identifier("capacity pool", pool)?;
                if *units == 0 || *limit == 0 || units > limit {
                    return Err(HostResourceError::InvalidRequest(
                        "capacity needs 0 < units <= limit".into(),
                    ));
                }
            }
            HostResourceKind::ExclusiveKey { name } => validate_identifier("exclusive key", name)?,
        }
    }
    Ok(())
}

fn validate_identifier(field: &str, value: &str) -> Result<(), HostResourceError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(HostResourceError::InvalidRequest(format!(
            "{field} must be 1..=256 printable characters"
        )));
    }
    Ok(())
}

fn validate_ttl(ttl: u64) -> Result<(), HostResourceError> {
    if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&ttl) {
        return Err(HostResourceError::InvalidRequest(format!(
            "ttl_seconds must be {MIN_TTL_SECONDS}..={MAX_TTL_SECONDS}"
        )));
    }
    Ok(())
}

fn request_digest(request: &HostResourceRequest) -> Result<String, HostResourceError> {
    let bytes = serde_json::to_vec(request)
        .map_err(|e| HostResourceError::InvalidRequest(e.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

struct Occupied {
    lease_id: String,
    generation: u64,
    kind: String,
    value: String,
    units: Option<u32>,
    limit: Option<u32>,
    holder_pid: Option<i64>,
    /// A named resource stays reserved after its holder dies, so the waiter needs
    /// to be told that waiting cannot resolve it.
    holder_gone: bool,
}

/// Quarantined leases keep occupying their resources, because reusing a name
/// whose cleanup was never confirmed is exactly what quarantine exists to
/// prevent. Capacity is the one exception, and only for a provably dead holder.
///
/// The two kinds reserve different things. A namespace or exclusive key names a
/// real artifact -- a database, a directory -- that may still hold residue, so it
/// stays reserved until reconciliation proves cleanup. A capacity unit is a pure
/// counter reserving machine throughput, with no artifact to leave dirty, so once
/// the process consuming that throughput is gone the unit reserves nothing at all.
/// Holding it merely stalls the pool (issue #139).
///
/// Liveness rather than state is the test on purpose: a quarantined holder that is
/// still running keeps its units, because it is still consuming the CPU and memory
/// the pool exists to bound.
fn load_occupied(conn: &Connection) -> Result<Vec<Occupied>, HostResourceError> {
    let mut stmt=conn.prepare("SELECT a.lease_id,l.generation,a.kind,a.value,a.units,a.capacity_limit,l.holder_pid FROM resource_allocations a JOIN resource_leases l ON l.lease_id=a.lease_id WHERE l.state IN ('active','quarantined') ORDER BY l.generation,a.resource_key")?;
    let rows = stmt
        .query_map([], |row| {
            let holder_pid = row.get::<_, Option<i64>>(6)?;
            Ok(Occupied {
                lease_id: row.get(0)?,
                generation: row.get::<_, i64>(1)? as u64,
                kind: row.get(2)?,
                value: row.get(3)?,
                units: row.get::<_, Option<i64>>(4)?.map(|v| v as u32),
                limit: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                holder_pid,
                holder_gone: holder_pid.is_some_and(holder_process_is_gone),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|occupied| {
            occupied.kind != "capacity" || !occupied.holder_pid.is_some_and(holder_process_is_gone)
        })
        .collect())
}

fn plan_allocations(
    request: &HostResourceRequest,
    occupied: &[Occupied],
) -> Result<(Vec<HostResourceAllocation>, Vec<HostResourceConflict>), HostResourceError> {
    let mut proposed = Vec::new();
    let mut conflicts = Vec::new();
    let seed = &request_digest(request)?[..12];
    for requirement in &request.resources {
        match &requirement.resource {
            HostResourceKind::Namespace { prefix } => proposed.push(allocation(
                &requirement.key,
                "namespace",
                format!("{}-{seed}", sanitize(prefix)),
                None,
                None,
            )),
            HostResourceKind::TcpPort { start, end } => {
                let port = (*start..=*end).find(|p| {
                    !occupied
                        .iter()
                        .any(|o| o.kind == "tcp_port" && o.value == p.to_string())
                        && TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, *p)).is_ok()
                });
                if let Some(port) = port {
                    proposed.push(allocation(
                        &requirement.key,
                        "tcp_port",
                        port.to_string(),
                        None,
                        None,
                    ));
                } else {
                    conflicts.push(conflict(
                        "resource_contention",
                        &requirement.key,
                        "tcp_port",
                        format!("{start}-{end}"),
                        "no broker-free and OS-bindable port remains",
                        None,
                    ));
                }
            }
            HostResourceKind::Capacity { pool, units, limit } => {
                let matching = occupied
                    .iter()
                    .filter(|o| o.kind == "capacity" && o.value == *pool)
                    .collect::<Vec<_>>();
                if let Some(owner) = matching.iter().find(|o| o.limit != Some(*limit)) {
                    conflicts.push(conflict(
                        "capacity_policy_mismatch",
                        &requirement.key,
                        "capacity",
                        format!("{pool}:{units}/{limit}"),
                        "active pool uses a different limit",
                        Some(owner.lease_id.clone()),
                    ));
                    continue;
                }
                let used = matching.iter().filter_map(|o| o.units).sum::<u32>();
                if used.saturating_add(*units) > *limit {
                    conflicts.push(conflict(
                        "resource_contention",
                        &requirement.key,
                        "capacity",
                        format!("{pool}:{units}/{limit}"),
                        &format!(
                            "pool has {used}/{limit} units allocated by {} live lease(s)",
                            matching.len()
                        ),
                        matching.first().map(|o| o.lease_id.clone()),
                    ));
                } else {
                    proposed.push(allocation(
                        &requirement.key,
                        "capacity",
                        pool.clone(),
                        Some(*units),
                        Some(*limit),
                    ));
                }
            }
            HostResourceKind::ExclusiveKey { name } => {
                if let Some(owner) = occupied
                    .iter()
                    .find(|o| o.kind == "exclusive_key" && o.value == *name)
                {
                    let reason = if owner.holder_gone {
                        let holder = owner.holder_pid.map_or_else(
                            || "holder PID unknown".into(),
                            |pid| format!("holder PID {pid}"),
                        );
                        format!(
                            "exclusive key is held by lease {} ({holder}) whose holder process is gone; \
                             waiting cannot release it, review cleanup and run `aethyme broker \
                             resources reconcile {} --confirm {}`",
                            owner.lease_id, owner.lease_id, owner.generation
                        )
                    } else {
                        format!("exclusive key is held by lease {}", owner.lease_id)
                    };
                    conflicts.push(conflict(
                        "resource_contention",
                        &requirement.key,
                        "exclusive_key",
                        name.clone(),
                        &reason,
                        Some(owner.lease_id.clone()),
                    ));
                } else {
                    proposed.push(allocation(
                        &requirement.key,
                        "exclusive_key",
                        name.clone(),
                        None,
                        None,
                    ));
                }
            }
        }
    }
    Ok((proposed, conflicts))
}

fn allocation(
    key: &str,
    kind: &str,
    value: String,
    units: Option<u32>,
    limit: Option<u32>,
) -> HostResourceAllocation {
    HostResourceAllocation {
        key: key.into(),
        kind: kind.into(),
        value,
        units,
        capacity_limit: limit,
    }
}
fn conflict(
    code: &str,
    key: &str,
    kind: &str,
    requested: String,
    reason: &str,
    owner: Option<String>,
) -> HostResourceConflict {
    HostResourceConflict {
        code: code.into(),
        resource_key: key.into(),
        kind: kind.into(),
        requested,
        reason: reason.into(),
        owning_lease: owner,
    }
}

fn leases_for_conflict(
    conflict: &HostResourceConflict,
    leases: &[HostResourceLease],
) -> Vec<HostResourceLease> {
    let matches = leases.iter().filter(|lease| {
        lease.allocations.iter().any(|allocation| {
            if allocation.kind != conflict.kind {
                return false;
            }
            match conflict.kind.as_str() {
                "capacity" => conflict
                    .requested
                    .split_once(':')
                    .is_some_and(|(pool, _)| allocation.value == pool),
                "exclusive_key" => allocation.value == conflict.requested,
                "tcp_port" => parse_port_range(&conflict.requested).is_some_and(|(start, end)| {
                    allocation
                        .value
                        .parse::<u16>()
                        .is_ok_and(|port| (start..=end).contains(&port))
                }),
                _ => false,
            }
        })
    });
    let mut selected = matches.cloned().collect::<Vec<_>>();
    if let Some(owner) = conflict.owning_lease.as_deref()
        && !selected.iter().any(|lease| lease.lease_id == owner)
        && let Some(lease) = leases.iter().find(|lease| lease.lease_id == owner)
    {
        selected.push(lease.clone());
    }
    selected
}

fn host_resource_holder(lease: &HostResourceLease) -> HostResourceHolder {
    HostResourceHolder {
        lease_id: lease.lease_id.clone(),
        generation: lease.generation,
        run_id: lease.run_id.clone(),
        repository: lease.repository.clone(),
        worktree_fingerprint: lease.worktree_fingerprint.clone(),
        state: lease.state,
        holder_pid: lease.holder_pid,
        process_alive: lease
            .holder_pid
            .map(|pid| !holder_process_is_gone(i64::from(pid))),
        expires_at: lease.expires_at,
    }
}

fn parse_port_range(value: &str) -> Option<(u16, u16)> {
    let (start, end) = value.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?))
}

fn any_port_bindable(value: &str) -> bool {
    let Some((start, end)) = parse_port_range(value) else {
        return false;
    };
    (start..=end)
        .any(|port| TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).is_ok())
}

fn blocker_recovery(conflict: &HostResourceConflict, leases: &[HostResourceLease]) -> String {
    if conflict.code == "capacity_policy_mismatch" {
        return "the pool limit differs from an existing holder; use the same limit or wait until the existing lease is released".into();
    }
    if let Some(lease) = leases.iter().find(|lease| {
        lease
            .holder_pid
            .is_some_and(|pid| holder_process_is_gone(i64::from(pid)))
    }) {
        let holder = lease.holder_pid.map_or_else(
            || "holder PID unknown".into(),
            |pid| format!("holder PID {pid}"),
        );
        return format!(
            "holder process for lease {} ({holder}) is gone; review cleanup, then run `aethyme broker resources reconcile {} --confirm {}`",
            lease.lease_id, lease.lease_id, lease.generation
        );
    }
    "ordinary contention; wait for the holder to release the lease and retry the same request"
        .into()
}

fn wait_advice(blockers: &[HostResourceBlocker]) -> HostResourceWaitAdvice {
    if blockers.is_empty() {
        return HostResourceWaitAdvice {
            waitable: true,
            reason: "no_conflict".into(),
            action: "no wait is required; acquire can proceed".into(),
        };
    }
    if blockers
        .iter()
        .any(|blocker| blocker.conflict.code == "capacity_policy_mismatch")
    {
        return HostResourceWaitAdvice {
            waitable: false,
            reason: "capacity_policy_mismatch".into(),
            action: "align the requested pool limit with the existing lease; waiting cannot change policy".into(),
        };
    }
    if let Some(holder) = blockers
        .iter()
        .flat_map(|blocker| &blocker.holders)
        .find(|holder| holder.process_alive == Some(false))
    {
        return HostResourceWaitAdvice {
            waitable: false,
            reason: "orphaned_holder".into(),
            action: format!(
                "review cleanup for lease {} (holder PID {}), then run `aethyme broker resources reconcile {} --confirm {}`",
                holder.lease_id,
                holder
                    .holder_pid
                    .map_or_else(|| "unknown".into(), |pid| pid.to_string()),
                holder.lease_id,
                holder.generation
            ),
        };
    }
    HostResourceWaitAdvice {
        waitable: true,
        reason: "resource_contention".into(),
        action: "wait for the identified lease(s) to release, then retry this request".into(),
    }
}

fn aggregate_conflict_code(conflicts: &[HostResourceConflict]) -> &'static str {
    if conflicts
        .iter()
        .any(|conflict| conflict.code == "capacity_policy_mismatch")
    {
        "capacity_policy_mismatch"
    } else {
        "resource_contention"
    }
}
fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .into()
}
fn quarantine_expired(conn: &Connection, now: i64) -> Result<(), HostResourceError> {
    conn.execute("UPDATE resource_leases SET state='quarantined',updated_at=?1 WHERE state='active' AND expires_at<=?1",[now])?;
    Ok(())
}

/// Probe with signal 0: it reports whether a process can be signalled without
/// disturbing a live holder.
///
/// Deliberately conservative about PID reuse. A recycled PID reports *alive*,
/// which forgoes an early reclaim; the opposite bias would revoke a lease from a
/// running holder. Only a provably absent process (`ESRCH`) counts as gone, so a
/// PID this process may not signal (`EPERM`) is treated as alive.
fn holder_process_is_gone(pid: i64) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return false;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// A holder that no longer exists can never renew or release its lease, so its
/// allocations pin the pool until the TTL elapses. On a pool whose limit a single
/// run consumes entirely, that stalls every gate on the machine -- across every
/// worktree and every session, not only the one that leaked (issue #139).
///
/// Reclaiming early is safe precisely because renewal comes from the holder
/// process: once it is gone nothing will renew, so quarantining only anticipates
/// an expiry that is already certain. Quarantine rather than release keeps the
/// existing reconciliation gate, so cleanup authority is still confirmed.
fn quarantine_dead_holders(conn: &Connection, now: i64) -> Result<(), HostResourceError> {
    let candidates = {
        let mut stmt = conn.prepare(
            "SELECT lease_id,holder_pid FROM resource_leases \
             WHERE state='active' AND holder_pid IS NOT NULL",
        )?;
        stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    for (lease_id, holder_pid) in candidates {
        if !holder_process_is_gone(holder_pid) {
            continue;
        }
        conn.execute(
            "UPDATE resource_leases SET state='quarantined',updated_at=?2 \
             WHERE lease_id=?1 AND state='active'",
            params![lease_id, now],
        )?;
    }
    Ok(())
}

/// Every write path quarantines stale holders and reclaims their unusable
/// capacity before reading occupancy, so a stalled pool frees itself at the
/// next attempt rather than requiring an operator to notice.
fn quarantine_reclaimable(
    conn: &Connection,
    now: i64,
) -> Result<HostResourceReapReport, HostResourceError> {
    quarantine_expired(conn, now)?;
    quarantine_dead_holders(conn, now)?;
    reclaim_dead_capacity(conn, now)
}

/// Reclaim only the allocation that becomes meaningless with the holder's
/// process: a capacity unit. Namespaces, ports, and exclusive keys remain on
/// the quarantined lease because they can correspond to residue on the host.
fn reclaim_dead_capacity(
    conn: &Connection,
    now: i64,
) -> Result<HostResourceReapReport, HostResourceError> {
    let candidates = {
        let mut stmt = conn.prepare(
            "SELECT lease_id,generation,holder_pid FROM resource_leases \
             WHERE state='quarantined' AND holder_pid IS NOT NULL \
             AND NOT EXISTS (\
                 SELECT 1 FROM resource_reap_observations r \
                 WHERE r.lease_id=resource_leases.lease_id\
             ) \
             ORDER BY generation",
        )?;
        stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? as u64,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    let mut report = HostResourceReapReport {
        schema_version: HOST_RESOURCE_SCHEMA_VERSION,
        dead_holders_seen: 0,
        reclaimed_capacity_units: 0,
        released_leases: 0,
        retained_quarantined_leases: 0,
        leases: Vec::new(),
    };
    for (lease_id, generation, holder_pid) in candidates {
        if !holder_process_is_gone(holder_pid) {
            continue;
        }
        let observed = conn.execute(
            "INSERT INTO resource_reap_observations(lease_id,generation,reaped_at) \
             VALUES (?1,?2,?3) ON CONFLICT(lease_id) DO NOTHING",
            params![lease_id, generation as i64, now],
        )?;
        if observed == 0 {
            continue;
        }
        report.dead_holders_seen += 1;
        let capacity_units = conn.query_row(
            "SELECT COALESCE(SUM(units), 0) FROM resource_allocations \
             WHERE lease_id=?1 AND kind='capacity'",
            [&lease_id],
            |row| row.get::<_, i64>(0),
        )? as u32;
        if capacity_units > 0 {
            conn.execute(
                "DELETE FROM resource_allocations WHERE lease_id=?1 AND kind='capacity'",
                [&lease_id],
            )?;
        }
        let remaining_allocations: i64 = conn.query_row(
            "SELECT COUNT(*) FROM resource_allocations WHERE lease_id=?1",
            [&lease_id],
            |row| row.get(0),
        )?;
        let state = if remaining_allocations == 0 {
            conn.execute(
                "UPDATE resource_leases SET state='released',released_at=?2,updated_at=?2 \
                 WHERE lease_id=?1 AND state='quarantined'",
                params![lease_id, now],
            )?;
            report.released_leases += 1;
            HostLeaseState::Released
        } else {
            conn.execute(
                "UPDATE resource_leases SET updated_at=?2 WHERE lease_id=?1 AND state='quarantined'",
                params![lease_id, now],
            )?;
            report.retained_quarantined_leases += 1;
            HostLeaseState::Quarantined
        };
        report.reclaimed_capacity_units = report
            .reclaimed_capacity_units
            .saturating_add(capacity_units);
        report.leases.push(HostResourceReapLease {
            lease_id,
            generation,
            holder_pid: holder_pid as u32,
            capacity_units,
            state,
        });
    }
    Ok(report)
}

fn verify_ownership(
    conn: &Connection,
    lease_id: &str,
    generation: u64,
    token: &str,
    active: bool,
) -> Result<(), HostResourceError> {
    let found: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT generation,ownership_token,state FROM resource_leases WHERE lease_id=?1",
            [lease_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((actual, stored, state)) = found else {
        return Err(HostResourceError::LeaseNotFound(lease_id.into()));
    };
    if actual as u64 != generation || stored != token {
        return Err(HostResourceError::OwnershipMismatch(lease_id.into()));
    }
    if active && state != "active" {
        return Err(HostResourceError::RequestNotActive {
            request_id: lease_id.into(),
            state,
        });
    }
    Ok(())
}

fn load_lease(
    conn: &Connection,
    selector: &str,
    value: &str,
) -> Result<Option<HostResourceLease>, HostResourceError> {
    let predicate = match selector {
        "lease_id" => "lease_id=?1",
        "request_id" => "request_id=?1",
        _ => return Err(HostResourceError::InvalidRequest("invalid selector".into())),
    };
    let sql = format!(
        "SELECT lease_id,request_id,repository,worktree_fingerprint,run_id,generation,state,holder_pid,created_at,expires_at,released_at FROM resource_leases WHERE {predicate}"
    );
    let row = conn
        .query_row(&sql, [value], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, Option<i64>>(10)?,
            ))
        })
        .optional()?;
    let Some((
        lease_id,
        request_id,
        repository,
        worktree_fingerprint,
        run_id,
        generation,
        state,
        holder_pid,
        created_at,
        expires_at,
        released_at,
    )) = row
    else {
        return Ok(None);
    };
    let mut stmt=conn.prepare("SELECT resource_key,kind,value,units,capacity_limit FROM resource_allocations WHERE lease_id=?1 ORDER BY resource_key")?;
    let allocations = stmt
        .query_map([&lease_id], |r| {
            Ok(HostResourceAllocation {
                key: r.get(0)?,
                kind: r.get(1)?,
                value: r.get(2)?,
                units: r.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                capacity_limit: r.get::<_, Option<i64>>(4)?.map(|v| v as u32),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(HostResourceLease {
        lease_id,
        request_id,
        repository,
        worktree_fingerprint,
        run_id,
        generation: generation as u64,
        state: HostLeaseState::parse(&state)?,
        holder_pid: holder_pid.map(|v| v as u32),
        created_at,
        expires_at,
        released_at,
        allocations,
    }))
}

fn random_hex(conn: &Connection, bytes: usize) -> Result<String, HostResourceError> {
    Ok(
        conn.query_row("SELECT lower(hex(randomblob(?1)))", [bytes as i64], |r| {
            r.get(0)
        })?,
    )
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn req(id: &str, resources: Vec<HostResourceRequirement>) -> HostResourceRequest {
        HostResourceRequest {
            schema_version: 1,
            request_id: id.into(),
            repository: "owner/repo".into(),
            worktree_fingerprint: "abc".into(),
            run_id: format!("run-{id}"),
            ttl_seconds: 60,
            holder_pid: Some(std::process::id()),
            resources,
        }
    }
    fn exclusive(name: &str) -> HostResourceRequirement {
        HostResourceRequirement {
            key: "database".into(),
            resource: HostResourceKind::ExclusiveKey { name: name.into() },
        }
    }

    #[cfg(unix)]
    #[test]
    fn durable_state_permissions_protect_ownership_credentials() {
        use std::os::unix::fs::PermissionsExt;

        let t = tempfile::tempdir().unwrap();
        let state = t.path().join("host-state");
        let database = state.join("h.db");
        HostResourceCoordinator::open(&database).unwrap();
        assert_eq!(
            std::fs::metadata(&state).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&database).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn bundle_is_atomic_and_request_is_idempotent() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let r = req("one", vec![exclusive("db")]);
        let a = c.acquire(&r).unwrap();
        assert_eq!(a, c.acquire(&r).unwrap());
        let r2 = req(
            "two",
            vec![
                HostResourceRequirement {
                    key: "ns".into(),
                    resource: HostResourceKind::Namespace {
                        prefix: "worker".into(),
                    },
                },
                exclusive("db"),
            ],
        );
        assert!(matches!(
            c.acquire(&r2),
            Err(HostResourceError::Conflict { .. })
        ));
        assert_eq!(c.list(true).unwrap().len(), 1);
    }
    #[test]
    fn credentials_fence_release() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let a = c.acquire(&req("one", vec![exclusive("db")])).unwrap();
        assert!(matches!(
            c.release(&a.lease.lease_id, a.lease.generation, "wrong"),
            Err(HostResourceError::OwnershipMismatch(_))
        ));
        assert_eq!(
            c.release(&a.lease.lease_id, a.lease.generation, &a.ownership_token)
                .unwrap()
                .state,
            HostLeaseState::Released
        );
        assert!(c.acquire(&req("two", vec![exclusive("db")])).is_ok());
    }
    #[test]
    fn capacity_is_enforced() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let cap = |id: &str| {
            req(
                id,
                vec![HostResourceRequirement {
                    key: "slot".into(),
                    resource: HostResourceKind::Capacity {
                        pool: "heavy".into(),
                        units: 1,
                        limit: 2,
                    },
                }],
            )
        };
        let a = c.acquire(&cap("one")).unwrap();
        assert_eq!(a.environment()["AETHYME_RESOURCE_SLOT"], "heavy");
        c.acquire(&cap("two")).unwrap();
        assert!(matches!(
            c.acquire(&cap("three")),
            Err(HostResourceError::Conflict { .. })
        ));
    }
    /// Reaped, so the PID is provably absent rather than merely idle.
    fn reaped_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    fn whole_pool(id: &str) -> HostResourceRequest {
        req(
            id,
            vec![HostResourceRequirement {
                key: "host-work".into(),
                resource: HostResourceKind::Capacity {
                    pool: "host-work".into(),
                    units: 4,
                    limit: 4,
                },
            }],
        )
    }

    fn slot_and_worktree(id: &str, holder_pid: u32) -> HostResourceRequest {
        let mut request = req(
            id,
            vec![
                HostResourceRequirement {
                    key: "prepush_slot".into(),
                    resource: HostResourceKind::Capacity {
                        pool: "prepush".into(),
                        units: 1,
                        limit: 3,
                    },
                },
                HostResourceRequirement {
                    key: "worktree".into(),
                    resource: HostResourceKind::ExclusiveKey {
                        name: "shared-worktree".into(),
                    },
                },
            ],
        );
        request.holder_pid = Some(holder_pid);
        request
    }

    /// Issue #139: a holder that died without releasing pinned a whole-pool lease
    /// until its TTL, stalling every gate on the machine.
    #[test]
    fn dead_holder_does_not_pin_the_pool_until_expiry() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let mut leaked = whole_pool("leaked");
        // An hour of TTL left, so expiry cannot be what frees the pool.
        leaked.ttl_seconds = 3_600;
        leaked.holder_pid = Some(reaped_pid());
        c.acquire(&leaked).unwrap();

        c.acquire(&whole_pool("next"))
            .expect("a dead holder must not pin the pool for the rest of its TTL");
    }

    /// The capacity exception must not leak into named resources: a dead holder
    /// may have left a database or directory dirty, so its exclusive key stays
    /// reserved until reconciliation proves cleanup.
    #[test]
    fn dead_holder_still_reserves_its_named_resources() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let mut leaked = req("leaked", vec![exclusive("shared")]);
        leaked.ttl_seconds = 3_600;
        leaked.holder_pid = Some(reaped_pid());
        c.acquire(&leaked).unwrap();

        let Err(HostResourceError::Conflict { message, .. }) =
            c.acquire(&req("next", vec![exclusive("shared")]))
        else {
            panic!("a dead holder's exclusive key must stay reserved");
        };
        // Waiting is futile here, so the message has to say so and name the remedy.
        assert!(
            message.contains("holder process is gone") && message.contains("reconcile"),
            "conflict must explain that waiting cannot resolve it: {message}"
        );
    }

    #[test]
    fn independent_reaper_reclaims_dead_capacity_but_keeps_named_lease_diagnosable() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let holder_pid = reaped_pid();
        let leaked = slot_and_worktree("leaked", holder_pid);
        let grant = c.acquire(&leaked).unwrap();
        c.quarantine(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        )
        .unwrap();

        let report = c.reap_dead_holders().unwrap();
        assert_eq!(report.dead_holders_seen, 1);
        assert_eq!(report.reclaimed_capacity_units, 1);
        assert_eq!(report.released_leases, 0);
        assert_eq!(report.retained_quarantined_leases, 1);
        assert_eq!(report.leases[0].holder_pid, holder_pid);
        assert_eq!(report.leases[0].state, HostLeaseState::Quarantined);

        let second = c.reap_dead_holders().unwrap();
        assert_eq!(second.dead_holders_seen, 0);
        assert_eq!(second.reclaimed_capacity_units, 0);
        assert_eq!(second.released_leases, 0);
        assert_eq!(second.retained_quarantined_leases, 0);
        assert!(second.leases.is_empty());

        let retained = c
            .list(false)
            .unwrap()
            .into_iter()
            .find(|lease| lease.lease_id == grant.lease.lease_id)
            .unwrap();
        assert_eq!(retained.state, HostLeaseState::Quarantined);
        assert_eq!(retained.allocations.len(), 1);
        assert_eq!(retained.allocations[0].kind, "exclusive_key");

        let explanation = c
            .explain(&slot_and_worktree("next", std::process::id()))
            .unwrap();
        assert_eq!(explanation.proposed.len(), 1);
        assert_eq!(explanation.blockers.len(), 1);
        let blocker = &explanation.blockers[0];
        assert!(blocker.conflict.reason.contains(&grant.lease.lease_id));
        assert!(
            blocker
                .conflict
                .reason
                .contains(&format!("holder PID {holder_pid}"))
        );
        assert!(blocker.conflict.reason.contains("process is gone"));
        assert!(
            blocker
                .conflict
                .reason
                .contains(&format!("--confirm {}", grant.lease.generation))
        );
        assert!(!explanation.wait.waitable);
        assert!(explanation.wait.action.contains(&grant.lease.lease_id));
    }

    #[test]
    fn independent_reaper_reports_named_only_dead_holder_once() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let holder_pid = reaped_pid();
        let mut request = req("named-only", vec![exclusive("shared")]);
        request.holder_pid = Some(holder_pid);
        let grant = c.acquire(&request).unwrap();
        c.quarantine(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        )
        .unwrap();

        let report = c.reap_dead_holders().unwrap();
        assert_eq!(report.dead_holders_seen, 1);
        assert_eq!(report.reclaimed_capacity_units, 0);
        assert_eq!(report.released_leases, 0);
        assert_eq!(report.retained_quarantined_leases, 1);
        assert_eq!(report.leases.len(), 1);
        assert_eq!(report.leases[0].lease_id, grant.lease.lease_id);
        assert_eq!(report.leases[0].generation, grant.lease.generation);
        assert_eq!(report.leases[0].holder_pid, holder_pid);
        assert_eq!(report.leases[0].capacity_units, 0);
        assert_eq!(report.leases[0].state, HostLeaseState::Quarantined);

        let second = c.reap_dead_holders().unwrap();
        assert_eq!(second.dead_holders_seen, 0);
        assert!(second.leases.is_empty());
    }

    /// The reclaim must not be so eager that it revokes a lease from a running
    /// holder; only a provably absent process is reclaimed.
    #[test]
    fn live_holder_keeps_its_capacity() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let mut held = whole_pool("held");
        held.holder_pid = Some(std::process::id());
        c.acquire(&held).unwrap();

        assert!(matches!(
            c.acquire(&whole_pool("next")),
            Err(HostResourceError::Conflict { .. })
        ));
    }

    #[test]
    fn invalid_request_never_writes() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let mut r = req("bad", vec![exclusive("db")]);
        r.ttl_seconds = 1;
        assert!(matches!(
            c.acquire(&r),
            Err(HostResourceError::InvalidRequest(_))
        ));
        assert!(c.list(true).unwrap().is_empty());
    }

    #[test]
    fn independent_connections_cannot_acquire_the_same_exclusive_key() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("h.db");
        let first = HostResourceCoordinator::open(&path).unwrap();
        let second = HostResourceCoordinator::open(&path).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles = [(first, "one"), (second, "two")].map(|(mut coordinator, id)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                coordinator.acquire(&req(id, vec![exclusive("shared")]))
            })
        });
        let results = handles.map(|handle| handle.join().unwrap());
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(HostResourceError::Conflict { .. })))
                .count(),
            1
        );
    }

    #[test]
    fn bounded_wait_acquires_after_the_owner_releases() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("h.db");
        let mut owner = HostResourceCoordinator::open(&path).unwrap();
        let grant = owner
            .acquire(&req("owner", vec![exclusive("shared")]))
            .unwrap();
        let mut waiter = HostResourceCoordinator::open(&path).unwrap();
        let waited = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = waited.clone();
        let handle = std::thread::spawn(move || {
            waiter.acquire_with_wait(
                &req("waiter", vec![exclusive("shared")]),
                std::time::Duration::from_secs(2),
                |_| {
                    observed.store(true, std::sync::atomic::Ordering::SeqCst);
                },
            )
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        owner
            .release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
            .unwrap();
        assert!(handle.join().unwrap().is_ok());
        assert!(waited.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn bounded_wait_times_out_with_the_original_conflict() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("h.db");
        let mut owner = HostResourceCoordinator::open(&path).unwrap();
        owner
            .acquire(&req("owner", vec![exclusive("shared")]))
            .unwrap();
        let mut waiter = HostResourceCoordinator::open(&path).unwrap();
        let started = std::time::Instant::now();
        let result = waiter.acquire_with_wait(
            &req("waiter", vec![exclusive("shared")]),
            std::time::Duration::from_millis(50),
            |_| {},
        );
        assert!(matches!(result, Err(HostResourceError::Conflict { .. })));
        assert!(started.elapsed() >= std::time::Duration::from_millis(50));
    }

    #[test]
    fn expiry_quarantines_until_generation_confirmed_reconciliation() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let grant = c.acquire(&req("one", vec![exclusive("shared")])).unwrap();
        c.conn
            .execute(
                "UPDATE resource_leases SET expires_at = 0 WHERE lease_id = ?1",
                [&grant.lease.lease_id],
            )
            .unwrap();
        assert!(matches!(
            c.acquire(&req("two", vec![exclusive("shared")])),
            Err(HostResourceError::Conflict { .. })
        ));
        assert_eq!(c.list(false).unwrap()[0].state, HostLeaseState::Quarantined);
        assert!(matches!(
            c.reconcile(&grant.lease.lease_id, grant.lease.generation + 1),
            Err(HostResourceError::GenerationMismatch { .. })
        ));
        c.reconcile(&grant.lease.lease_id, grant.lease.generation)
            .unwrap();
        assert!(c.acquire(&req("three", vec![exclusive("shared")])).is_ok());
    }
}
