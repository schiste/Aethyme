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

mod managed_storage_inventory;
pub(crate) use managed_storage_inventory::ManagedStorageOwner;

pub const HOST_RESOURCE_SCHEMA_VERSION: u32 = 1;
/// Additive ledger version; the shared host-resource schema remains version 1.
pub(crate) const MANAGED_STORAGE_SCHEMA_VERSION: u32 = 1;
pub const HOST_RESOURCE_REQUEST_SCHEMA_VERSION: u32 = 1;
const MIN_TTL_SECONDS: u64 = 15;
const MAX_TTL_SECONDS: u64 = 86_400;
/// Bound each repository's durable managed-storage ledger, including policy
/// rows and released claim tombstones retained for idempotency. This is a
/// per-repository bound; it does not constrain unrelated repositories sharing
/// the host resource database.
const MAX_MANAGED_STORAGE_ROWS_PER_REPOSITORY: i64 = 16_384;

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

CREATE TABLE IF NOT EXISTS managed_storage_meta (
 key TEXT PRIMARY KEY,
 value INTEGER NOT NULL
);
INSERT OR IGNORE INTO managed_storage_meta VALUES ('schema_version', 1);
CREATE TABLE IF NOT EXISTS managed_storage_policies (
 repository TEXT NOT NULL,
 filesystem_id TEXT NOT NULL,
 managed_cap_bytes INTEGER NOT NULL CHECK (managed_cap_bytes >= 0),
 free_reserve_bytes INTEGER NOT NULL CHECK (free_reserve_bytes >= 0),
 policy_digest TEXT NOT NULL,
 updated_at INTEGER NOT NULL,
 PRIMARY KEY (repository, filesystem_id)
);
CREATE TABLE IF NOT EXISTS managed_storage_claims (
 repository TEXT NOT NULL,
 filesystem_id TEXT NOT NULL,
 claim_id TEXT NOT NULL,
 request_digest TEXT NOT NULL,
 policy_digest TEXT NOT NULL,
 policy_cap_bytes INTEGER NOT NULL CHECK (policy_cap_bytes >= 0),
 policy_free_reserve_bytes INTEGER NOT NULL CHECK (policy_free_reserve_bytes >= 0),
 materialization TEXT NOT NULL,
 root_identity TEXT NOT NULL,
 state TEXT NOT NULL CHECK (state IN ('reserved','charged','uncertain','cleanup_pending','released')),
 requested_bytes INTEGER NOT NULL CHECK (requested_bytes > 0),
 reserved_bytes INTEGER NOT NULL CHECK (reserved_bytes >= 0),
 measured_bytes INTEGER CHECK (measured_bytes IS NULL OR measured_bytes >= 0),
 release_receipt_digest TEXT,
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,
 PRIMARY KEY (repository, filesystem_id, claim_id),
 CHECK (state != 'reserved' OR measured_bytes IS NULL),
 CHECK (state != 'charged' OR (reserved_bytes = 0 AND measured_bytes IS NOT NULL)),
 CHECK (state != 'cleanup_pending' OR (reserved_bytes = 0 AND measured_bytes IS NOT NULL)),
 CHECK (state != 'released' OR release_receipt_digest IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS managed_storage_claims_by_filesystem_state
 ON managed_storage_claims(filesystem_id, state);

-- Additive lineage metadata keeps the shared host-resource schema at v1.
-- It records charged-root mutation epochs without changing old claim rows.
CREATE TABLE IF NOT EXISTS managed_storage_claim_lineage (
 repository TEXT NOT NULL,
 filesystem_id TEXT NOT NULL,
 claim_id TEXT NOT NULL,
 predecessor_claim_id TEXT,
 baseline_bytes INTEGER NOT NULL CHECK (baseline_bytes >= 0),
 root_instance_id TEXT,
 superseded_by_claim_id TEXT,
 updated_at INTEGER NOT NULL,
 PRIMARY KEY (repository, filesystem_id, claim_id),
 UNIQUE (repository, filesystem_id, predecessor_claim_id),
 UNIQUE (repository, filesystem_id, superseded_by_claim_id),
 CHECK (predecessor_claim_id IS NULL OR predecessor_claim_id != claim_id),
 CHECK (superseded_by_claim_id IS NULL OR superseded_by_claim_id != claim_id),
 FOREIGN KEY (repository,filesystem_id,claim_id)
   REFERENCES managed_storage_claims(repository,filesystem_id,claim_id),
 FOREIGN KEY (repository,filesystem_id,predecessor_claim_id)
   REFERENCES managed_storage_claims(repository,filesystem_id,claim_id),
 FOREIGN KEY (repository,filesystem_id,superseded_by_claim_id)
   REFERENCES managed_storage_claims(repository,filesystem_id,claim_id)
);
CREATE INDEX IF NOT EXISTS managed_storage_lineage_by_predecessor
 ON managed_storage_claim_lineage(repository,filesystem_id,predecessor_claim_id);

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

/// Explicit per-repository, per-filesystem policy for the dormant managed
/// storage ledger. It does not constrain arbitrary child-process writes.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ManagedStoragePolicy {
    /// Stable repository identity, never a worktree path.
    pub repository: String,
    /// Stable filesystem identity supplied by the caller.
    pub filesystem_id: String,
    /// Maximum measured bytes plus outstanding reservations.
    pub managed_cap_bytes: u64,
    /// Minimum free bytes to retain after outstanding reservations.
    pub free_reserve_bytes: u64,
}

/// A durable reservation request. root_identity is an opaque stable identity,
/// not a path. Claims have no PID lease or expiry.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ManagedStorageClaimRequest {
    pub repository: String,
    pub filesystem_id: String,
    pub claim_id: String,
    pub materialization: String,
    pub root_identity: String,
    pub reserved_bytes: u64,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManagedStorageClaimState {
    Reserved,
    Charged,
    Uncertain,
    CleanupPending,
    Released,
}

impl ManagedStorageClaimState {
    fn parse(value: &str) -> Result<Self, HostResourceError> {
        match value {
            "reserved" => Ok(Self::Reserved),
            "charged" => Ok(Self::Charged),
            "uncertain" => Ok(Self::Uncertain),
            "cleanup_pending" => Ok(Self::CleanupPending),
            "released" => Ok(Self::Released),
            other => Err(HostResourceError::InvalidRequest(format!(
                "unknown managed storage claim state {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct ManagedStorageClaim {
    pub request: ManagedStorageClaimRequest,
    /// Exact explicit policy snapshot that admitted this reservation.
    pub policy: ManagedStoragePolicy,
    pub state: ManagedStorageClaimState,
    pub policy_digest: String,
    pub request_digest: String,
    pub measured_bytes: Option<u64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedStorageLineage {
    predecessor_claim_id: Option<String>,
    baseline_bytes: u64,
    root_instance_id: Option<String>,
    superseded_by_claim_id: Option<String>,
}

#[derive(Debug, Clone)]
struct ManagedStorageMutationBaseline {
    predecessor_claim_id: String,
    measured_bytes: u64,
    root_instance_id: String,
}

/// Creating a durable reservation is distinct from finding an existing
/// idempotency key. `Created` means this call inserted the reservation;
/// `Existing` is recovery status and must not be treated as permission to
/// repeat materialization.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) enum ManagedStorageReservationResult {
    Created(ManagedStorageClaim),
    Existing(ManagedStorageClaim),
}

/// Created only after the no-follow reclaimer verifies complete removal of
/// the exact pinned root instance recorded by the claim lineage.
#[derive(Debug, Clone)]
pub(crate) struct VerifiedManagedStorageRemoval {
    repository: String,
    filesystem_id: String,
    claim_id: String,
    root_identity: String,
    root_instance_id: String,
    receipt_digest: String,
}

impl VerifiedManagedStorageRemoval {
    fn after_complete_no_follow_removal(
        repository: String,
        filesystem_id: String,
        claim_id: String,
        root_identity: String,
        root_instance_id: Option<String>,
        receipt_digest: String,
        remaining_bytes: u64,
        complete: bool,
    ) -> Result<Self, HostResourceError> {
        validate_managed_storage_identifier("repository", &repository)?;
        validate_managed_storage_identifier("filesystem_id", &filesystem_id)?;
        validate_managed_storage_identifier("claim_id", &claim_id)?;
        validate_managed_storage_identifier("root_identity", &root_identity)?;
        let root_instance_id = root_instance_id.ok_or_else(|| {
            managed_storage_refusal(
                "managed_storage_removal_instance_missing",
                "release requires a pinned root instance; absent roots need a separate typed proof",
            )
        })?;
        validate_managed_storage_identifier("root_instance_id", &root_instance_id)?;
        validate_managed_storage_identifier("receipt_digest", &receipt_digest)?;
        if !complete || remaining_bytes != 0 {
            return Err(managed_storage_refusal(
                "managed_storage_removal_unverified",
                "release requires complete no-follow removal of the exact claim root",
            ));
        }
        Ok(Self {
            repository,
            filesystem_id,
            claim_id,
            root_identity,
            root_instance_id,
            receipt_digest,
        })
    }
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
        Self::open_with_busy_timeout(path, std::time::Duration::from_secs(5))
    }

    /// Open a writable coordinator with a caller-bounded SQLite lock wait.
    /// Ordinary host-resource clients keep the five-second default above;
    /// short inline maintenance lanes can use a smaller timeout so database
    /// contention does not outlive their own deadline.
    pub(crate) fn open_with_busy_timeout(
        path: &Path,
        busy_timeout: std::time::Duration,
    ) -> Result<Self, HostResourceError> {
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
        conn.busy_timeout(busy_timeout)?;
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        validate_schema(&conn)?;
        validate_managed_storage_schema(&conn)?;
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

    /// Hold the exact preparation-cache exclusive lease while managing one
    /// finite materialization directory. The caller supplies the resolved
    /// preparation-cache root from this broker's storage context; this API does
    /// not constrain arbitrary child-process writes or prove child quiescence.
    pub(crate) fn begin_managed_storage_owner<'a>(
        &'a mut self,
        grant: HostResourceGrant,
        repository: &str,
        preparation_cache_root: &Path,
        materialization: &str,
        ttl_seconds: u64,
    ) -> Result<ManagedStorageOwner<'a>, HostResourceError> {
        ManagedStorageOwner::new(
            self,
            grant,
            repository,
            preparation_cache_root,
            materialization,
            ttl_seconds,
        )
    }

    /// Read one exact durable claim so a restarted owner can distinguish an
    /// outstanding reservation from measured, uncertain, or released storage.
    pub(crate) fn managed_storage_claim(
        &self,
        repository: &str,
        filesystem_id: &str,
        claim_id: &str,
    ) -> Result<Option<ManagedStorageClaim>, HostResourceError> {
        validate_managed_storage_identifier("repository", repository)?;
        validate_managed_storage_identifier("filesystem_id", filesystem_id)?;
        validate_managed_storage_identifier("claim_id", claim_id)?;
        load_managed_storage_claim(&self.conn, repository, filesystem_id, claim_id)
    }

    fn backfill_managed_storage_lineage(
        &mut self,
        request: &ManagedStorageClaimRequest,
        measured_bytes: u64,
        root_instance_id: &str,
    ) -> Result<ManagedStorageClaim, HostResourceError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let claim = load_managed_storage_claim(
            &tx,
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )?
        .ok_or_else(|| {
            managed_storage_refusal("managed_storage_claim_missing", "claim does not exist")
        })?;
        if claim.request != *request
            || claim.state != ManagedStorageClaimState::Charged
            || claim.measured_bytes != Some(measured_bytes)
        {
            return Err(managed_storage_refusal(
                "managed_storage_backfill_mismatch",
                "backfill requires the exact current charged claim and byte measurement",
            ));
        }
        if let Some(lineage) = load_managed_storage_lineage(
            &tx,
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )? {
            if lineage.superseded_by_claim_id.is_some()
                || lineage
                    .root_instance_id
                    .as_deref()
                    .is_some_and(|known| known != root_instance_id)
            {
                return Err(managed_storage_refusal(
                    "managed_storage_backfill_stale",
                    "the claim is superseded or the root instance differs from its recorded lineage",
                ));
            }
            if lineage.root_instance_id.as_deref() != Some(root_instance_id) {
                tx.execute(
                    "UPDATE managed_storage_claim_lineage
                     SET root_instance_id=?4,baseline_bytes=?5,updated_at=?6
                     WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3",
                    params![
                        request.repository,
                        request.filesystem_id,
                        request.claim_id,
                        root_instance_id,
                        managed_storage_i64("baseline_bytes", measured_bytes)?,
                        now
                    ],
                )?;
            }
        } else {
            let other: Option<String> = tx
                .query_row(
                    "SELECT claim_id FROM managed_storage_claims
                     WHERE repository=?1 AND filesystem_id=?2 AND materialization=?3
                       AND root_identity=?4 AND claim_id!=?5
                       AND state IN ('charged','cleanup_pending') LIMIT 1",
                    params![
                        request.repository,
                        request.filesystem_id,
                        request.materialization,
                        request.root_identity,
                        request.claim_id
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(other) = other {
                return Err(managed_storage_refusal(
                    "managed_storage_backfill_ambiguous",
                    &format!("another measured claim {other:?} names this root"),
                ));
            }
            tx.execute(
                "INSERT INTO managed_storage_claim_lineage
                 (repository,filesystem_id,claim_id,predecessor_claim_id,baseline_bytes,
                  root_instance_id,superseded_by_claim_id,updated_at)
                 VALUES (?1,?2,?3,NULL,?4,?5,NULL,?6)",
                params![
                    request.repository,
                    request.filesystem_id,
                    request.claim_id,
                    managed_storage_i64("baseline_bytes", measured_bytes)?,
                    root_instance_id,
                    now
                ],
            )?;
        }
        let claim = load_managed_storage_claim(
            &tx,
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )?
        .ok_or_else(|| {
            HostResourceError::InvalidRequest(
                "managed storage claim disappeared during lineage backfill".into(),
            )
        })?;
        tx.commit()?;
        Ok(claim)
    }

    /// Install or update an explicit policy. Lowering a cap preserves existing
    /// charges; future reservations refuse until usage fits the new cap.
    pub(crate) fn set_managed_storage_policy(
        &mut self,
        policy: &ManagedStoragePolicy,
    ) -> Result<(), HostResourceError> {
        self.set_managed_storage_policy_with_row_limit(
            policy,
            MAX_MANAGED_STORAGE_ROWS_PER_REPOSITORY,
        )
    }

    fn set_managed_storage_policy_with_row_limit(
        &mut self,
        policy: &ManagedStoragePolicy,
        row_limit: i64,
    ) -> Result<(), HostResourceError> {
        validate_managed_storage_policy(policy)?;
        let digest = managed_storage_policy_digest(policy)?;
        let cap = managed_storage_i64("managed_cap_bytes", policy.managed_cap_bytes)?;
        let reserve = managed_storage_i64("free_reserve_bytes", policy.free_reserve_bytes)?;
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM managed_storage_policies
             WHERE repository=?1 AND filesystem_id=?2)",
            params![policy.repository, policy.filesystem_id],
            |row| row.get(0),
        )?;
        if !exists {
            ensure_managed_storage_row_capacity(&tx, &policy.repository, row_limit)?;
        }
        tx.execute(
            "INSERT INTO managed_storage_policies
             (repository,filesystem_id,managed_cap_bytes,free_reserve_bytes,policy_digest,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(repository,filesystem_id) DO UPDATE SET
             managed_cap_bytes=excluded.managed_cap_bytes,
             free_reserve_bytes=excluded.free_reserve_bytes,
             policy_digest=excluded.policy_digest,updated_at=excluded.updated_at",
            params![policy.repository, policy.filesystem_id, cap, reserve, digest, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Reserve against the configured managed cap and a current free-space
    /// probe. The probe runs only after this method has acquired the immediate
    /// transaction, so another broker reservation cannot settle between the
    /// observation and admission. The filesystem can still be changed by
    /// unrelated writers after the observation; this is not a physical quota.
    fn reserve_managed_storage_with_probe<F>(
        &mut self,
        request: &ManagedStorageClaimRequest,
        free_space: F,
    ) -> Result<ManagedStorageReservationResult, HostResourceError>
    where
        F: FnOnce() -> Result<u64, HostResourceError>,
    {
        self.reserve_managed_storage_with_row_limit_and_probe(
            request,
            free_space,
            MAX_MANAGED_STORAGE_ROWS_PER_REPOSITORY,
        )
    }

    fn reserve_managed_storage_with_row_limit_and_probe<F>(
        &mut self,
        request: &ManagedStorageClaimRequest,
        free_space: F,
        row_limit: i64,
    ) -> Result<ManagedStorageReservationResult, HostResourceError>
    where
        F: FnOnce() -> Result<u64, HostResourceError>,
    {
        self.reserve_managed_storage_with_baseline(request, free_space, row_limit, None)
    }

    fn reserve_managed_storage_mutation_with_probe<F>(
        &mut self,
        request: &ManagedStorageClaimRequest,
        free_space: F,
        baseline: ManagedStorageMutationBaseline,
    ) -> Result<ManagedStorageReservationResult, HostResourceError>
    where
        F: FnOnce() -> Result<u64, HostResourceError>,
    {
        self.reserve_managed_storage_with_baseline(
            request,
            free_space,
            MAX_MANAGED_STORAGE_ROWS_PER_REPOSITORY,
            Some(baseline),
        )
    }

    // Test fixtures inject free-space readings through the same callback path
    // used by production. These byte helpers are unavailable in non-test builds.
    #[cfg(test)]
    fn reserve_managed_storage(
        &mut self,
        request: &ManagedStorageClaimRequest,
        free_bytes: u64,
    ) -> Result<ManagedStorageReservationResult, HostResourceError> {
        self.reserve_managed_storage_with_probe(request, || Ok(free_bytes))
    }

    #[cfg(test)]
    fn reserve_managed_storage_with_row_limit(
        &mut self,
        request: &ManagedStorageClaimRequest,
        free_bytes: u64,
        row_limit: i64,
    ) -> Result<ManagedStorageReservationResult, HostResourceError> {
        self.reserve_managed_storage_with_row_limit_and_probe(request, || Ok(free_bytes), row_limit)
    }

    fn reserve_managed_storage_with_baseline<F>(
        &mut self,
        request: &ManagedStorageClaimRequest,
        free_space: F,
        row_limit: i64,
        baseline: Option<ManagedStorageMutationBaseline>,
    ) -> Result<ManagedStorageReservationResult, HostResourceError>
    where
        F: FnOnce() -> Result<u64, HostResourceError>,
    {
        validate_managed_storage_claim_request(request)?;
        let request_digest = managed_storage_claim_digest(request)?;
        let requested_i64 = managed_storage_i64("reserved_bytes", request.reserved_bytes)?;
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let existing = load_managed_storage_claim(
            &tx,
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )?;
        if let Some(claim) = &existing {
            if claim.request_digest != request_digest || claim.request != *request {
                return Err(HostResourceError::IdempotencyMismatch(
                    request.claim_id.clone(),
                ));
            }
            let lineage = load_managed_storage_lineage(
                &tx,
                &request.repository,
                &request.filesystem_id,
                &request.claim_id,
            )?;
            if lineage
                .as_ref()
                .and_then(|row| row.predecessor_claim_id.as_deref())
                != baseline
                    .as_ref()
                    .map(|value| value.predecessor_claim_id.as_str())
            {
                return Err(HostResourceError::IdempotencyMismatch(
                    request.claim_id.clone(),
                ));
            }
            tx.commit()?;
            return Ok(ManagedStorageReservationResult::Existing(claim.clone()));
        }

        if let Some(baseline) = &baseline {
            let previous = load_managed_storage_claim(
                &tx,
                &request.repository,
                &request.filesystem_id,
                &baseline.predecessor_claim_id,
            )?
            .ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_predecessor_missing",
                    "the charged predecessor claim does not exist",
                )
            })?;
            let previous_lineage = load_managed_storage_lineage(
                &tx,
                &request.repository,
                &request.filesystem_id,
                &baseline.predecessor_claim_id,
            )?;
            if previous.state != ManagedStorageClaimState::Charged
                || previous.measured_bytes != Some(baseline.measured_bytes)
                || previous.request.materialization != request.materialization
                || previous.request.root_identity != request.root_identity
                || previous_lineage.as_ref().is_none_or(|row| {
                    row.superseded_by_claim_id.is_some()
                        || row.root_instance_id.as_deref()
                            != Some(baseline.root_instance_id.as_str())
                })
            {
                return Err(managed_storage_refusal(
                    "managed_storage_predecessor_stale",
                    "predecessor is not the current charge for this exact root instance",
                ));
            }
        }

        ensure_managed_storage_row_capacity(&tx, &request.repository, row_limit)?;

        let policy = load_managed_storage_policy(&tx, &request.repository, &request.filesystem_id)?
            .ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_policy_missing",
                    "an explicit repository and filesystem storage cap is required",
                )
            })?;

        let unresolved: Option<(String, String, String)> = tx
            .query_row(
                "SELECT repository,claim_id,state FROM managed_storage_claims
             WHERE filesystem_id=?1 AND state IN ('reserved','uncertain') LIMIT 1",
                [&request.filesystem_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((repository, claim_id, state)) = unresolved {
            let (code, detail) = if state == "uncertain" {
                (
                    "managed_storage_inventory_uncertain",
                    "has an uncertain size",
                )
            } else {
                (
                    "managed_storage_materialization_pending",
                    "has an unresolved materialization reservation",
                )
            };
            return Err(managed_storage_refusal(
                code,
                &format!(
                    "claim {claim_id:?} for repository {repository:?} on this filesystem {detail}; reconcile it before admitting another allocation"
                ),
            ));
        }

        let managed_used = managed_storage_sum(
            &tx,
            "SELECT COALESCE(SUM(CASE
                 WHEN state='released' OR EXISTS (
                     SELECT 1 FROM managed_storage_claim_lineage l
                     WHERE l.repository=managed_storage_claims.repository
                       AND l.filesystem_id=managed_storage_claims.filesystem_id
                       AND l.claim_id=managed_storage_claims.claim_id
                       AND l.superseded_by_claim_id IS NOT NULL
                 ) THEN 0
                 WHEN state='reserved' THEN reserved_bytes
                 ELSE COALESCE(measured_bytes,reserved_bytes)
             END),0)
             FROM managed_storage_claims WHERE repository=?1 AND filesystem_id=?2",
            params![request.repository, request.filesystem_id],
        )?;
        let added_managed_bytes = request.reserved_bytes;
        let projected = managed_used
            .checked_add(added_managed_bytes)
            .ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_budget_overflow",
                    "managed storage arithmetic overflowed",
                )
            })?;
        if projected > policy.policy.managed_cap_bytes {
            return Err(managed_storage_refusal(
                "managed_storage_budget_exceeded",
                &format!(
                    "request would use {projected} bytes against an explicit {} byte cap",
                    policy.policy.managed_cap_bytes
                ),
            ));
        }

        let outstanding = managed_storage_sum(
            &tx,
            "SELECT COALESCE(SUM(reserved_bytes),0)
             FROM managed_storage_claims WHERE filesystem_id=?1 AND state='reserved'",
            [&request.filesystem_id],
        )?;
        let added_free_reservation = request.reserved_bytes;
        let required_free = policy
            .policy
            .free_reserve_bytes
            .checked_add(outstanding)
            .and_then(|value| value.checked_add(added_free_reservation))
            .ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_reserve_overflow",
                    "filesystem reserve arithmetic overflowed",
                )
            })?;
        // Keep the fallible filesystem sample inside this transaction. Exact
        // retries returned above without probing; new admissions observe disk
        // only after serializing against other broker claim settlement.
        let free_bytes = free_space()?;
        if free_bytes < required_free {
            return Err(managed_storage_refusal(
                "managed_storage_free_reserve",
                &format!(
                    "request needs {required_free} free bytes including outstanding reservations; observed {free_bytes}"
                ),
            ));
        }

        let policy_digest = policy.policy_digest.clone();
        tx.execute(
            "INSERT INTO managed_storage_claims
             (repository,filesystem_id,claim_id,request_digest,policy_digest,
              policy_cap_bytes,policy_free_reserve_bytes,materialization,root_identity,
              state,requested_bytes,reserved_bytes,measured_bytes,release_receipt_digest,
              created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'reserved',?10,?10,NULL,NULL,?11,?11)",
            params![
                request.repository,
                request.filesystem_id,
                request.claim_id,
                request_digest,
                policy_digest,
                managed_storage_i64("managed_cap_bytes", policy.policy.managed_cap_bytes)?,
                managed_storage_i64("free_reserve_bytes", policy.policy.free_reserve_bytes)?,
                request.materialization,
                request.root_identity,
                requested_i64,
                now
            ],
        )?;
        let (predecessor_claim_id, baseline_bytes, root_instance_id) = baseline
            .map(|baseline| {
                (
                    Some(baseline.predecessor_claim_id),
                    baseline.measured_bytes,
                    Some(baseline.root_instance_id),
                )
            })
            .unwrap_or((None, 0, None));
        tx.execute(
            "INSERT INTO managed_storage_claim_lineage
             (repository,filesystem_id,claim_id,predecessor_claim_id,baseline_bytes,
              root_instance_id,superseded_by_claim_id,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,NULL,?7)",
            params![
                request.repository,
                request.filesystem_id,
                request.claim_id,
                predecessor_claim_id,
                managed_storage_i64("baseline_bytes", baseline_bytes)?,
                root_instance_id,
                now
            ],
        )?;
        let claim = load_managed_storage_claim(
            &tx,
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )?
        .ok_or_else(|| {
            HostResourceError::InvalidRequest(
                "managed storage reservation disappeared inside its transaction".into(),
            )
        })?;
        tx.commit()?;
        Ok(ManagedStorageReservationResult::Created(claim))
    }

    /// Record a complete inventory of a present root. Actual use is retained
    /// even above policy, so over-budget bytes cannot be hidden.
    fn reconcile_managed_storage_claim(
        &mut self,
        request: &ManagedStorageClaimRequest,
        measured_bytes: u64,
        root_instance_id: Option<&str>,
        root_present: bool,
    ) -> Result<ManagedStorageClaim, HostResourceError> {
        validate_managed_storage_claim_request(request)?;
        let measured = managed_storage_i64("measured_bytes", measured_bytes)?;
        let digest = managed_storage_claim_digest(request)?;
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut claim = load_managed_storage_claim(
            &tx,
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )?
        .ok_or_else(|| {
            managed_storage_refusal("managed_storage_claim_missing", "claim does not exist")
        })?;
        if claim.request_digest != digest || claim.request != *request {
            return Err(HostResourceError::IdempotencyMismatch(
                request.claim_id.clone(),
            ));
        }
        let lineage = load_managed_storage_lineage(
            &tx,
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )?
        .ok_or_else(|| {
            managed_storage_refusal(
                "managed_storage_lineage_missing",
                "reservation has no mutation lineage row",
            )
        })?;
        match claim.state {
            ManagedStorageClaimState::Charged
                if claim.measured_bytes == Some(measured_bytes)
                    && lineage.root_instance_id.as_deref() == root_instance_id =>
            {
                tx.commit()?;
                return Ok(claim);
            }
            ManagedStorageClaimState::Reserved | ManagedStorageClaimState::Uncertain => {}
            _ => {
                return Err(managed_storage_refusal(
                    "managed_storage_claim_state",
                    "only reserved or uncertain claims can be reconciled as present storage",
                ));
            }
        }
        if let Some(predecessor_id) = &lineage.predecessor_claim_id {
            if !root_present
                || root_instance_id.is_none()
                || root_instance_id != lineage.root_instance_id.as_deref()
            {
                return Err(managed_storage_refusal(
                    "managed_storage_mutation_root_changed",
                    "the charged root disappeared or was replaced during its mutation epoch",
                ));
            }
            let predecessor = load_managed_storage_claim(
                &tx,
                &request.repository,
                &request.filesystem_id,
                predecessor_id,
            )?
            .ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_predecessor_missing",
                    "the charged predecessor claim disappeared before settlement",
                )
            })?;
            let predecessor_lineage = load_managed_storage_lineage(
                &tx,
                &request.repository,
                &request.filesystem_id,
                predecessor_id,
            )?
            .ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_predecessor_lineage_missing",
                    "the charged predecessor has no lineage record",
                )
            })?;
            if predecessor.state != ManagedStorageClaimState::Charged
                || predecessor.measured_bytes != Some(lineage.baseline_bytes)
                || predecessor.request.materialization != request.materialization
                || predecessor.request.root_identity != request.root_identity
                || predecessor_lineage.superseded_by_claim_id.is_some()
                || predecessor_lineage.root_instance_id.as_deref() != root_instance_id
            {
                return Err(managed_storage_refusal(
                    "managed_storage_predecessor_stale",
                    "the predecessor charge no longer matches the mutation baseline",
                ));
            }
            let linked = tx.execute(
                "UPDATE managed_storage_claim_lineage
                 SET superseded_by_claim_id=?4,updated_at=?5
                 WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3
                   AND superseded_by_claim_id IS NULL",
                params![
                    request.repository,
                    request.filesystem_id,
                    predecessor_id,
                    request.claim_id,
                    now
                ],
            )?;
            if linked != 1 {
                return Err(managed_storage_refusal(
                    "managed_storage_predecessor_stale",
                    "predecessor was superseded before mutation settlement",
                ));
            }
        }
        tx.execute(
            "UPDATE managed_storage_claims SET state='charged',reserved_bytes=0,
             measured_bytes=?4,updated_at=?5
             WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3",
            params![
                request.repository,
                request.filesystem_id,
                request.claim_id,
                measured,
                now
            ],
        )?;
        let lineage_updated = tx.execute(
            "UPDATE managed_storage_claim_lineage SET root_instance_id=?4,updated_at=?5
             WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3",
            params![
                request.repository,
                request.filesystem_id,
                request.claim_id,
                root_instance_id,
                now
            ],
        )?;
        if lineage_updated != 1 {
            return Err(managed_storage_refusal(
                "managed_storage_lineage_missing",
                "the reservation lineage disappeared before settlement",
            ));
        }
        claim = load_managed_storage_claim(
            &tx,
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )?
        .ok_or_else(|| {
            HostResourceError::InvalidRequest(
                "managed storage claim disappeared during reconciliation".into(),
            )
        })?;
        tx.commit()?;
        Ok(claim)
    }

    /// Preserve an allocation when its complete size or cleanup cannot be
    /// proved. Uncertain claims block new allocations for their repo/filesystem.
    pub(crate) fn mark_managed_storage_claim_uncertain(
        &mut self,
        request: &ManagedStorageClaimRequest,
    ) -> Result<ManagedStorageClaim, HostResourceError> {
        validate_managed_storage_claim_request(request)?;
        let digest = managed_storage_claim_digest(request)?;
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut claim = load_managed_storage_claim(
            &tx,
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )?
        .ok_or_else(|| {
            managed_storage_refusal("managed_storage_claim_missing", "claim does not exist")
        })?;
        if claim.request_digest != digest || claim.request != *request {
            return Err(HostResourceError::IdempotencyMismatch(
                request.claim_id.clone(),
            ));
        }
        if claim.state == ManagedStorageClaimState::Released {
            return Err(managed_storage_refusal(
                "managed_storage_claim_released",
                "a released claim cannot become uncertain",
            ));
        }
        if claim.state != ManagedStorageClaimState::Uncertain {
            tx.execute(
                "UPDATE managed_storage_claims SET state='uncertain',updated_at=?4
                 WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3",
                params![
                    request.repository,
                    request.filesystem_id,
                    request.claim_id,
                    now
                ],
            )?;
            claim = load_managed_storage_claim(
                &tx,
                &request.repository,
                &request.filesystem_id,
                &request.claim_id,
            )?
            .ok_or_else(|| {
                HostResourceError::InvalidRequest(
                    "managed storage claim disappeared while marking it uncertain".into(),
                )
            })?;
        }
        tx.commit()?;
        Ok(claim)
    }

    /// Keep a measured charge while its exact root awaits verified cleanup.
    pub(crate) fn mark_managed_storage_cleanup_pending(
        &mut self,
        request: &ManagedStorageClaimRequest,
    ) -> Result<ManagedStorageClaim, HostResourceError> {
        validate_managed_storage_claim_request(request)?;
        let digest = managed_storage_claim_digest(request)?;
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut claim = load_managed_storage_claim(
            &tx,
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )?
        .ok_or_else(|| {
            managed_storage_refusal("managed_storage_claim_missing", "claim does not exist")
        })?;
        if claim.request_digest != digest || claim.request != *request {
            return Err(HostResourceError::IdempotencyMismatch(
                request.claim_id.clone(),
            ));
        }
        if load_managed_storage_lineage(
            &tx,
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )?
        .is_some_and(|lineage| lineage.superseded_by_claim_id.is_some())
        {
            return Err(managed_storage_refusal(
                "managed_storage_release_not_current",
                "only the current charged-root lineage tip can enter cleanup-pending state",
            ));
        }
        match claim.state {
            ManagedStorageClaimState::CleanupPending => {
                tx.commit()?;
                return Ok(claim);
            }
            ManagedStorageClaimState::Charged => {}
            _ => {
                return Err(managed_storage_refusal(
                    "managed_storage_claim_state",
                    "only a measured charge can enter cleanup-pending state",
                ));
            }
        }
        tx.execute(
            "UPDATE managed_storage_claims SET state='cleanup_pending',updated_at=?4
             WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3",
            params![
                request.repository,
                request.filesystem_id,
                request.claim_id,
                now
            ],
        )?;
        claim = load_managed_storage_claim(
            &tx,
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )?
        .ok_or_else(|| {
            HostResourceError::InvalidRequest(
                "managed storage claim disappeared while queuing cleanup".into(),
            )
        })?;
        tx.commit()?;
        Ok(claim)
    }

    /// Drop a charge only after complete no-follow removal of its exact root.
    pub(crate) fn release_managed_storage_claim(
        &mut self,
        receipt: &VerifiedManagedStorageRemoval,
    ) -> Result<ManagedStorageClaim, HostResourceError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut claim = load_managed_storage_claim(
            &tx,
            &receipt.repository,
            &receipt.filesystem_id,
            &receipt.claim_id,
        )?
        .ok_or_else(|| {
            managed_storage_refusal("managed_storage_claim_missing", "claim does not exist")
        })?;
        if claim.request.root_identity != receipt.root_identity {
            return Err(managed_storage_refusal(
                "managed_storage_root_mismatch",
                "removal receipt does not match the claim root",
            ));
        }
        let mut child_lineage = load_managed_storage_lineage(
            &tx,
            &receipt.repository,
            &receipt.filesystem_id,
            &receipt.claim_id,
        )?
        .ok_or_else(|| {
            managed_storage_refusal(
                "managed_storage_lineage_missing",
                "release requires pinned lineage for the exact root instance",
            )
        })?;
        if child_lineage.root_instance_id.as_deref() != Some(receipt.root_instance_id.as_str()) {
            return Err(managed_storage_refusal(
                "managed_storage_root_instance_mismatch",
                "removal receipt does not match the root instance recorded for this claim",
            ));
        }
        if claim.state == ManagedStorageClaimState::Released {
            let previous: Option<String> = tx.query_row(
                "SELECT release_receipt_digest FROM managed_storage_claims
                 WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3",
                params![receipt.repository, receipt.filesystem_id, receipt.claim_id],
                |row| row.get(0),
            )?;
            if previous.as_deref() == Some(receipt.receipt_digest.as_str()) {
                tx.commit()?;
                return Ok(claim);
            }
            return Err(managed_storage_refusal(
                "managed_storage_release_replay_mismatch",
                "claim was released with another receipt",
            ));
        }
        if claim.state != ManagedStorageClaimState::CleanupPending {
            return Err(managed_storage_refusal(
                "managed_storage_claim_state",
                "only cleanup-pending claims can be released",
            ));
        }
        let mut chain = vec![(
            receipt.claim_id.clone(),
            ManagedStorageClaimState::CleanupPending,
        )];
        let mut child_id = receipt.claim_id.clone();
        loop {
            if chain.len() > MAX_MANAGED_STORAGE_ROWS_PER_REPOSITORY as usize {
                return Err(managed_storage_refusal(
                    "managed_storage_lineage_too_deep",
                    "root mutation lineage exceeds the bounded repository row limit",
                ));
            }
            if child_lineage.root_instance_id.as_deref() != Some(receipt.root_instance_id.as_str())
            {
                return Err(managed_storage_refusal(
                    "managed_storage_root_instance_mismatch",
                    "removal receipt does not match the root instance recorded throughout its lineage",
                ));
            }
            if child_id == receipt.claim_id && child_lineage.superseded_by_claim_id.is_some() {
                return Err(managed_storage_refusal(
                    "managed_storage_release_not_current",
                    "verified removal must refer to the current tip of the charged-root lineage",
                ));
            }
            let Some(predecessor_id) = child_lineage.predecessor_claim_id.clone() else {
                break;
            };
            let predecessor = load_managed_storage_claim(
                &tx,
                &receipt.repository,
                &receipt.filesystem_id,
                &predecessor_id,
            )?
            .ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_lineage_predecessor_missing",
                    "verified root lineage has a missing predecessor claim",
                )
            })?;
            let predecessor_lineage = load_managed_storage_lineage(
                &tx,
                &receipt.repository,
                &receipt.filesystem_id,
                &predecessor_id,
            )?
            .ok_or_else(|| {
                managed_storage_refusal(
                    "managed_storage_lineage_predecessor_missing",
                    "verified root lineage has a missing predecessor metadata row",
                )
            })?;
            if predecessor.request.root_identity != receipt.root_identity
                || predecessor.request.materialization != claim.request.materialization
                || predecessor.state != ManagedStorageClaimState::Charged
                || predecessor.measured_bytes != Some(child_lineage.baseline_bytes)
                || predecessor_lineage.root_instance_id.as_deref()
                    != Some(receipt.root_instance_id.as_str())
                || predecessor_lineage.superseded_by_claim_id.as_deref() != Some(child_id.as_str())
            {
                return Err(managed_storage_refusal(
                    "managed_storage_lineage_invalid",
                    "verified root lineage has a mismatched root instance, byte baseline, or predecessor edge",
                ));
            }
            child_id = predecessor_id.clone();
            child_lineage = predecessor_lineage;
            chain.push((predecessor_id, ManagedStorageClaimState::Charged));
        }
        for (claim_id, expected_state) in &chain {
            let changed = tx.execute(
                "UPDATE managed_storage_claims
                 SET state='released',reserved_bytes=0,release_receipt_digest=?4,updated_at=?5
                 WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3 AND state=?6",
                params![
                    receipt.repository,
                    receipt.filesystem_id,
                    claim_id,
                    receipt.receipt_digest,
                    now,
                    match expected_state {
                        ManagedStorageClaimState::CleanupPending => "cleanup_pending",
                        ManagedStorageClaimState::Charged => "charged",
                        _ => unreachable!("lineage release only includes charged rows"),
                    }
                ],
            )?;
            if changed != 1 {
                return Err(managed_storage_refusal(
                    "managed_storage_lineage_changed",
                    "a charged-root claim changed before atomic lineage release",
                ));
            }
        }
        claim = load_managed_storage_claim(
            &tx,
            &receipt.repository,
            &receipt.filesystem_id,
            &receipt.claim_id,
        )?
        .ok_or_else(|| {
            HostResourceError::InvalidRequest(
                "managed storage claim disappeared during verified release".into(),
            )
        })?;
        tx.commit()?;
        Ok(claim)
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
        quarantine_reclaimable(&tx, now)?;
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

    /// Read an existing grant for an exact durable request after a caller crash.
    /// This never acquires capacity, renews a lease or changes its generation.
    /// The caller must already own its private recovery journal and run lock.
    pub(crate) fn recover_request(
        &self,
        request: &HostResourceRequest,
    ) -> Result<Option<HostResourceGrant>, HostResourceError> {
        validate_request(request)?;
        let digest = request_digest(request)?;
        let found = self
            .conn
            .query_row(
                "SELECT request_digest,ownership_token FROM resource_leases WHERE request_id=?1",
                [&request.request_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((existing_digest, token)) = found else {
            return Ok(None);
        };
        if existing_digest != digest {
            return Err(HostResourceError::IdempotencyMismatch(
                request.request_id.clone(),
            ));
        }
        let lease = load_lease(&self.conn, "request_id", &request.request_id)?
            .ok_or_else(|| HostResourceError::LeaseNotFound(request.request_id.clone()))?;
        Ok(Some(HostResourceGrant {
            lease,
            ownership_token: token,
        }))
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
        quarantine_reclaimable(&tx, now)?;
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
        quarantine_reclaimable(&tx, now)?;
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
                crate::warn_unrecorded(
                    "quarantine the host resource lease",
                    self.quarantine(
                        &grant.lease.lease_id,
                        grant.lease.generation,
                        &grant.ownership_token,
                    ),
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
                crate::warn_unrecorded(
                    "quarantine the host resource lease",
                    self.quarantine(
                        &grant.lease.lease_id,
                        grant.lease.generation,
                        &grant.ownership_token,
                    ),
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
                    crate::warn_unrecorded(
                        "quarantine the host resource lease",
                        self.quarantine(
                            &grant.lease.lease_id,
                            grant.lease.generation,
                            &grant.ownership_token,
                        ),
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
        quarantine_reclaimable(&tx, now)?;
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
        // SAFETY: `libc::sigset_t` is a plain-old-data C struct of integers, so
        // every bit pattern is valid for it and `zeroed` is the empty set. Both
        // locals stay in scope and exclusively borrowed until the calls below
        // return.
        let mut watched = unsafe { std::mem::zeroed() };
        // SAFETY: as above — `sigset_t` is POD.
        let mut previous = unsafe { std::mem::zeroed() };
        // SAFETY: `watched` and `previous` are live, writable, correctly
        // aligned `sigset_t`s that outlive these calls. The `sig*` functions
        // take them by pointer and only read or fill the set, and
        // `pthread_sigmask` stores the previous mask through `previous` before
        // returning.
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
        // SAFETY: `libc::sigset_t` is POD; `zeroed` is the empty set and
        // `pending` is a live, writable local.
        let mut pending = unsafe { std::mem::zeroed() };
        // SAFETY: `pending` is a live, writable, correctly aligned `sigset_t`
        // that outlives the call; `sigpending` only fills it.
        if unsafe { libc::sigpending(&mut pending) } != 0 {
            return None;
        }
        let has_watched = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
            .into_iter()
            // SAFETY: `pending` is a live, correctly aligned `sigset_t` and
            // `signal` is a valid `c_int` constant; `sigismember` only reads the
            // set and returns membership.
            .any(|signal| unsafe { libc::sigismember(&pending, signal) } == 1);
        if !has_watched {
            return None;
        }
        let mut signal = 0;
        // SAFETY: `self.watched` is a live `sigset_t` owned by `self`, and
        // `signal` is a live, writable `c_int` local. `sigwait` only reads the
        // set and writes the delivered signal number through `signal`.
        (unsafe { libc::sigwait(&self.watched, &mut signal) } == 0).then_some(signal)
    }
}

#[cfg(unix)]
impl Drop for BlockedSignals {
    fn drop(&mut self) {
        // SAFETY: `self.previous` is a live `sigset_t` recorded by `new`, and
        // the third argument is an explicit null out-pointer, which
        // `pthread_sigmask` documents as allowed when oldset is null. Restoring
        // the mask in `drop` touches only this thread's mask.
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
        // SAFETY: the closure runs in the child between fork and exec. It
        // restores the signal mask that `BlockedSignals::new` recorded in the
        // parent and converts errno into an `io::Error`; it allocates, locks
        // nothing, and runs no Rust destructor, so it is async-signal-safe.
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
            // SAFETY: `process_group(0)` was called before spawn, so the child
            // leads a new process group whose id is its pid, and `child` is
            // still live here. `killpg` takes both by value and dereferences
            // nothing this crate owns.
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
                    // SAFETY: as above — the child leads the process group
                    // created before spawn and is still live at this point.
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

fn validate_managed_storage_identifier(field: &str, value: &str) -> Result<(), HostResourceError> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(HostResourceError::InvalidRequest(format!(
            "managed storage {field} must be 1-512 non-control characters"
        )));
    }
    Ok(())
}

fn validate_managed_storage_policy(policy: &ManagedStoragePolicy) -> Result<(), HostResourceError> {
    validate_managed_storage_identifier("repository", &policy.repository)?;
    validate_managed_storage_identifier("filesystem_id", &policy.filesystem_id)?;
    managed_storage_i64("managed_cap_bytes", policy.managed_cap_bytes)?;
    managed_storage_i64("free_reserve_bytes", policy.free_reserve_bytes)?;
    Ok(())
}

fn validate_managed_storage_claim_request(
    request: &ManagedStorageClaimRequest,
) -> Result<(), HostResourceError> {
    validate_managed_storage_identifier("repository", &request.repository)?;
    validate_managed_storage_identifier("filesystem_id", &request.filesystem_id)?;
    validate_managed_storage_identifier("claim_id", &request.claim_id)?;
    validate_managed_storage_identifier("materialization", &request.materialization)?;
    validate_managed_storage_identifier("root_identity", &request.root_identity)?;
    if request.reserved_bytes == 0 {
        return Err(HostResourceError::InvalidRequest(
            "managed storage reservation must be greater than zero".into(),
        ));
    }
    managed_storage_i64("reserved_bytes", request.reserved_bytes)?;
    Ok(())
}

fn managed_storage_i64(field: &str, value: u64) -> Result<i64, HostResourceError> {
    i64::try_from(value).map_err(|_| {
        HostResourceError::InvalidRequest(format!(
            "managed storage {field} exceeds SQLite signed integer range"
        ))
    })
}

fn managed_storage_policy_digest(
    policy: &ManagedStoragePolicy,
) -> Result<String, HostResourceError> {
    let bytes = serde_json::to_vec(policy)
        .map_err(|error| HostResourceError::InvalidRequest(error.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn managed_storage_claim_digest(
    request: &ManagedStorageClaimRequest,
) -> Result<String, HostResourceError> {
    let bytes = serde_json::to_vec(request)
        .map_err(|error| HostResourceError::InvalidRequest(error.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn managed_storage_refusal(code: &str, message: &str) -> HostResourceError {
    HostResourceError::Conflict {
        code: code.to_owned(),
        message: message.to_owned(),
        conflicts: Vec::new(),
    }
}

#[derive(Debug, Clone)]
struct ManagedStoragePolicyRow {
    policy: ManagedStoragePolicy,
    policy_digest: String,
}

fn load_managed_storage_policy(
    conn: &Connection,
    repository: &str,
    filesystem_id: &str,
) -> Result<Option<ManagedStoragePolicyRow>, HostResourceError> {
    conn.query_row(
        "SELECT repository,filesystem_id,managed_cap_bytes,free_reserve_bytes,policy_digest
         FROM managed_storage_policies WHERE repository=?1 AND filesystem_id=?2",
        params![repository, filesystem_id],
        |row| {
            let cap: i64 = row.get(2)?;
            let reserve: i64 = row.get(3)?;
            Ok(ManagedStoragePolicyRow {
                policy: ManagedStoragePolicy {
                    repository: row.get(0)?,
                    filesystem_id: row.get(1)?,
                    managed_cap_bytes: sql_u64(cap)?,
                    free_reserve_bytes: sql_u64(reserve)?,
                },
                policy_digest: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(HostResourceError::from)
}

fn load_managed_storage_claim(
    conn: &Connection,
    repository: &str,
    filesystem_id: &str,
    claim_id: &str,
) -> Result<Option<ManagedStorageClaim>, HostResourceError> {
    conn.query_row(
        "SELECT repository,filesystem_id,claim_id,materialization,root_identity,
                state,requested_bytes,measured_bytes,request_digest,policy_digest,
                policy_cap_bytes,policy_free_reserve_bytes,created_at,updated_at
         FROM managed_storage_claims
         WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3",
        params![repository, filesystem_id, claim_id],
        |row| {
            let state_text: String = row.get(5)?;
            let measured: Option<i64> = row.get(7)?;
            Ok(ManagedStorageClaim {
                request: ManagedStorageClaimRequest {
                    repository: row.get(0)?,
                    filesystem_id: row.get(1)?,
                    claim_id: row.get(2)?,
                    materialization: row.get(3)?,
                    root_identity: row.get(4)?,
                    reserved_bytes: sql_u64(row.get(6)?)?,
                },
                policy: ManagedStoragePolicy {
                    repository: row.get(0)?,
                    filesystem_id: row.get(1)?,
                    managed_cap_bytes: sql_u64(row.get(10)?)?,
                    free_reserve_bytes: sql_u64(row.get(11)?)?,
                },
                state: ManagedStorageClaimState::parse(&state_text).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        5,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                policy_digest: row.get(9)?,
                request_digest: row.get(8)?,
                measured_bytes: measured.map(sql_u64).transpose()?,
                created_at: row.get(12)?,
                updated_at: row.get(13)?,
            })
        },
    )
    .optional()
    .map_err(HostResourceError::from)
}

fn load_managed_storage_lineage(
    conn: &Connection,
    repository: &str,
    filesystem_id: &str,
    claim_id: &str,
) -> Result<Option<ManagedStorageLineage>, HostResourceError> {
    conn.query_row(
        "SELECT predecessor_claim_id,baseline_bytes,root_instance_id,superseded_by_claim_id
         FROM managed_storage_claim_lineage
         WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3",
        params![repository, filesystem_id, claim_id],
        |row| {
            let baseline: i64 = row.get(1)?;
            Ok(ManagedStorageLineage {
                predecessor_claim_id: row.get(0)?,
                baseline_bytes: sql_u64(baseline)?,
                root_instance_id: row.get(2)?,
                superseded_by_claim_id: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(HostResourceError::from)
}

fn sql_u64(value: i64) -> Result<u64, rusqlite::Error> {
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
}

fn managed_storage_sum<P>(conn: &Connection, sql: &str, params: P) -> Result<u64, HostResourceError>
where
    P: rusqlite::Params,
{
    let value: i64 = conn.query_row(sql, params, |row| row.get(0))?;
    u64::try_from(value).map_err(|_| {
        HostResourceError::InvalidRequest(
            "managed storage ledger contains a negative byte count".into(),
        )
    })
}

fn ensure_managed_storage_row_capacity(
    conn: &Connection,
    repository: &str,
    row_limit: i64,
) -> Result<(), HostResourceError> {
    if row_limit < 0 {
        return Err(HostResourceError::InvalidRequest(
            "managed storage row limit cannot be negative".into(),
        ));
    }
    let rows: i64 = conn.query_row(
        "SELECT
             (SELECT COUNT(*) FROM managed_storage_policies WHERE repository=?1)
             + (SELECT COUNT(*) FROM managed_storage_claims WHERE repository=?1)",
        [repository],
        |row| row.get(0),
    )?;
    if rows >= row_limit {
        return Err(managed_storage_refusal(
            "managed_storage_ledger_capacity",
            &format!(
                "repository {repository:?} already has {rows} managed-storage ledger rows; the per-repository limit is {row_limit}, and claim tombstones are retained for idempotency"
            ),
        ));
    }
    Ok(())
}

fn validate_managed_storage_schema(conn: &Connection) -> Result<(), HostResourceError> {
    let version: i64 = conn.query_row(
        "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
        [],
        |row| row.get(0),
    )?;
    if version != i64::from(MANAGED_STORAGE_SCHEMA_VERSION) {
        return Err(HostResourceError::InvalidRequest(format!(
            "unsupported managed storage schema {version}"
        )));
    }
    Ok(())
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
                             advanced resources reconcile {} --confirm {}`",
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
            "holder process for lease {} ({holder}) is gone; review cleanup, then run `aethyme broker advanced resources reconcile {} --confirm {}`",
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
                "review cleanup for lease {} (holder PID {}), then run `aethyme broker advanced resources reconcile {} --confirm {}`",
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
    // SAFETY: `kill` with signal 0 checks existence and permission without
    // delivering a signal, taking the pid by value. It dereferences no pointer
    // this crate owns, so `pid` only has to be a valid i32.
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

    fn managed_policy(
        repository: &str,
        filesystem_id: &str,
        cap: u64,
        reserve: u64,
    ) -> ManagedStoragePolicy {
        ManagedStoragePolicy {
            repository: repository.into(),
            filesystem_id: filesystem_id.into(),
            managed_cap_bytes: cap,
            free_reserve_bytes: reserve,
        }
    }

    fn managed_claim(claim_id: &str, bytes: u64) -> ManagedStorageClaimRequest {
        ManagedStorageClaimRequest {
            repository: "aethyme-main".into(),
            filesystem_id: "device-42".into(),
            claim_id: claim_id.into(),
            materialization: "session-worktree".into(),
            root_identity: format!("root:{claim_id}"),
            reserved_bytes: bytes,
        }
    }

    fn conflict_code(error: HostResourceError) -> String {
        match error {
            HostResourceError::Conflict { code, .. } => code,
            other => panic!("expected managed-storage refusal, got {other:?}"),
        }
    }

    fn newly_reserved(result: ManagedStorageReservationResult) -> ManagedStorageClaim {
        match result {
            ManagedStorageReservationResult::Created(claim) => claim,
            ManagedStorageReservationResult::Existing(claim) => {
                panic!("existing claim did not grant a new materialization: {claim:?}")
            }
        }
    }

    fn existing_claim(result: ManagedStorageReservationResult) -> ManagedStorageClaim {
        match result {
            ManagedStorageReservationResult::Existing(claim) => claim,
            ManagedStorageReservationResult::Created(claim) => {
                panic!("claim unexpectedly granted a new materialization: {claim:?}")
            }
        }
    }

    #[test]
    fn managed_storage_tables_are_additive_to_host_schema_v1() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host.db");
        let legacy = Connection::open(&db).unwrap();
        legacy
            .execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
             INSERT INTO meta VALUES ('schema_version', 1);",
            )
            .unwrap();
        drop(legacy);

        let coordinator = HostResourceCoordinator::open(&db).unwrap();
        let host_version: i64 = coordinator
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let ledger_version: i64 = coordinator
            .conn
            .query_row(
                "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(host_version, i64::from(HOST_RESOURCE_SCHEMA_VERSION));
        assert_eq!(ledger_version, i64::from(MANAGED_STORAGE_SCHEMA_VERSION));
    }

    #[test]
    fn managed_storage_row_limit_counts_policies_and_released_claim_tombstones() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host.db");
        let policy = managed_policy("aethyme-main", "device-42", 100, 0);
        let first = managed_claim("first", 1);
        let second_policy = managed_policy("aethyme-main", "device-43", 100, 0);

        let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
        coordinator
            .set_managed_storage_policy_with_row_limit(&policy, 2)
            .unwrap();
        let first_claim = newly_reserved(
            coordinator
                .reserve_managed_storage_with_row_limit(&first, 100, 2)
                .unwrap(),
        );
        assert_eq!(first_claim.state, ManagedStorageClaimState::Reserved);
        coordinator
            .mark_managed_storage_claim_uncertain(&first)
            .unwrap();
        coordinator
            .reconcile_managed_storage_claim(&first, 1, Some("test-instance:first"), true)
            .unwrap();
        coordinator
            .mark_managed_storage_cleanup_pending(&first)
            .unwrap();
        let removal = VerifiedManagedStorageRemoval::after_complete_no_follow_removal(
            first.repository.clone(),
            first.filesystem_id.clone(),
            first.claim_id.clone(),
            first.root_identity.clone(),
            Some("test-instance:first".into()),
            "receipt-first".into(),
            0,
            true,
        )
        .unwrap();
        coordinator.release_managed_storage_claim(&removal).unwrap();

        assert_eq!(
            existing_claim(
                coordinator
                    .reserve_managed_storage_with_row_limit(&first, 100, 2)
                    .unwrap()
            )
            .state,
            ManagedStorageClaimState::Released
        );
        assert_eq!(
            conflict_code(
                coordinator
                    .set_managed_storage_policy_with_row_limit(&second_policy, 2)
                    .unwrap_err()
            ),
            "managed_storage_ledger_capacity"
        );
        assert_eq!(
            conflict_code(
                coordinator
                    .reserve_managed_storage_with_row_limit(&managed_claim("second", 1), 100, 2,)
                    .unwrap_err()
            ),
            "managed_storage_ledger_capacity"
        );
        coordinator
            .set_managed_storage_policy_with_row_limit(
                &managed_policy("aethyme-main", "device-42", 90, 0),
                2,
            )
            .unwrap();
    }

    #[test]
    fn managed_storage_free_space_probe_is_serialized_and_failure_rolls_back() {
        use std::time::Duration;

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host.db");
        let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
        coordinator
            .set_managed_storage_policy(&managed_policy("aethyme-main", "device-42", 1_000, 50))
            .unwrap();

        // Writer A completes before B enters its transaction. Its measured use
        // has reduced the injected current free-space reading from 100 to 60.
        let first = managed_claim("space-writer-a", 40);
        newly_reserved(
            coordinator
                .reserve_managed_storage_with_probe(&first, || Ok(100))
                .unwrap(),
        );
        coordinator
            .reconcile_managed_storage_claim(&first, 40, Some("test-instance:space-writer-a"), true)
            .unwrap();

        let second = managed_claim("space-writer-b", 20);
        let competing_connection = Connection::open(&db).unwrap();
        competing_connection.busy_timeout(Duration::ZERO).unwrap();
        let second_error = coordinator
            .reserve_managed_storage_with_probe(&second, || {
                // The sample callback runs while this connection owns
                // BEGIN IMMEDIATE. A competing broker transaction cannot
                // settle between this observation and B's admission decision.
                let error = competing_connection
                    .execute_batch("BEGIN IMMEDIATE")
                    .expect_err("free-space probe ran outside the immediate transaction");
                assert!(matches!(error, rusqlite::Error::SqliteFailure(..)));
                Ok(60)
            })
            .unwrap_err();
        assert_eq!(conflict_code(second_error), "managed_storage_free_reserve");
        assert!(
            coordinator
                .managed_storage_claim("aethyme-main", "device-42", &second.claim_id)
                .unwrap()
                .is_none()
        );

        let third = managed_claim("space-writer-c", 1);
        assert_eq!(
            conflict_code(
                coordinator
                    .reserve_managed_storage_with_probe(&third, || {
                        Err(managed_storage_refusal(
                            "managed_storage_free_bytes_unknown",
                            "injected free-space probe failure",
                        ))
                    })
                    .unwrap_err()
            ),
            "managed_storage_free_bytes_unknown"
        );
        assert!(
            coordinator
                .managed_storage_claim("aethyme-main", "device-42", &third.claim_id)
                .unwrap()
                .is_none()
        );

        // An exact-key retry remains status-only and never requires a fresh
        // filesystem observation.
        assert!(matches!(
            coordinator
                .reserve_managed_storage_with_probe(&first, || {
                    panic!("exact retry unexpectedly sampled free space")
                })
                .unwrap(),
            ManagedStorageReservationResult::Existing(_)
        ));
    }

    #[test]
    fn managed_storage_reservations_are_atomic_idempotent_and_durable() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host.db");
        let policy = managed_policy("aethyme-main", "device-42", 80, 20);
        let first = managed_claim("session-1", 50);
        {
            let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
            coordinator.set_managed_storage_policy(&policy).unwrap();
            let reserved =
                newly_reserved(coordinator.reserve_managed_storage(&first, 200).unwrap());
            assert_eq!(reserved.state, ManagedStorageClaimState::Reserved);
            assert_eq!(reserved.request, first);
            assert_eq!(reserved.policy, policy);
            assert_eq!(
                existing_claim(coordinator.reserve_managed_storage(&first, 1).unwrap()),
                reserved,
            );

            let too_large = managed_claim("session-too-large", 36);
            assert_eq!(
                conflict_code(
                    coordinator
                        .reserve_managed_storage(&too_large, 200)
                        .unwrap_err()
                ),
                "managed_storage_materialization_pending"
            );

            let other_repository = ManagedStorageClaimRequest {
                repository: "another-repository".into(),
                ..managed_claim("other-repository-session", 1)
            };
            coordinator
                .set_managed_storage_policy(&managed_policy(
                    "another-repository",
                    "device-42",
                    100,
                    0,
                ))
                .unwrap();
            assert_eq!(
                conflict_code(
                    coordinator
                        .reserve_managed_storage(&other_repository, 200)
                        .unwrap_err()
                ),
                "managed_storage_materialization_pending"
            );
        }

        let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
        let reopened = existing_claim(coordinator.reserve_managed_storage(&first, 1).unwrap());
        assert_eq!(reopened.state, ManagedStorageClaimState::Reserved);
        assert_eq!(reopened.request, first);
        assert_eq!(reopened.policy, policy);

        coordinator
            .reconcile_managed_storage_claim(&first, 45, Some("test-instance:session-1"), true)
            .unwrap();

        let too_large = managed_claim("session-too-large", 36);
        assert_eq!(
            conflict_code(
                coordinator
                    .reserve_managed_storage(&too_large, 200)
                    .unwrap_err()
            ),
            "managed_storage_budget_exceeded"
        );

        let second = managed_claim("session-2", 30);
        assert_eq!(
            conflict_code(
                coordinator
                    .reserve_managed_storage(&second, 49)
                    .unwrap_err()
            ),
            "managed_storage_free_reserve"
        );
        let second = newly_reserved(coordinator.reserve_managed_storage(&second, 50).unwrap());
        assert_eq!(second.state, ManagedStorageClaimState::Reserved);

        coordinator
            .reconcile_managed_storage_claim(
                &second.request,
                30,
                Some("test-instance:session-2"),
                true,
            )
            .unwrap();
        let third = managed_claim("session-3", 6);
        assert_eq!(
            conflict_code(
                coordinator
                    .reserve_managed_storage(&third, 100)
                    .unwrap_err()
            ),
            "managed_storage_budget_exceeded"
        );
    }

    #[test]
    fn uncertain_managed_storage_survives_reopen_and_blocks_admission() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host.db");
        let policy = managed_policy("aethyme-main", "device-42", 100, 0);
        let first = managed_claim("uncertain-session", 40);
        {
            let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
            coordinator.set_managed_storage_policy(&policy).unwrap();
            newly_reserved(coordinator.reserve_managed_storage(&first, 1000).unwrap());
            let unknown = coordinator
                .mark_managed_storage_claim_uncertain(&first)
                .unwrap();
            assert_eq!(unknown.state, ManagedStorageClaimState::Uncertain);
        }

        let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
        let next = managed_claim("next-session", 1);
        assert_eq!(
            conflict_code(
                coordinator
                    .reserve_managed_storage(&next, 1000)
                    .unwrap_err()
            ),
            "managed_storage_inventory_uncertain"
        );
        let measured = coordinator
            .reconcile_managed_storage_claim(&first, 41, Some("test-instance:uncertain"), true)
            .unwrap();
        assert_eq!(measured.state, ManagedStorageClaimState::Charged);
        assert_eq!(measured.measured_bytes, Some(41));
        let next = newly_reserved(coordinator.reserve_managed_storage(&next, 1000).unwrap());
        assert_eq!(next.state, ManagedStorageClaimState::Reserved);
    }

    #[test]
    fn managed_storage_charge_releases_only_after_exact_verified_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host.db");
        let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
        coordinator
            .set_managed_storage_policy(&managed_policy("aethyme-main", "device-42", 100, 0))
            .unwrap();
        let request = managed_claim("cleanup-session", 40);
        newly_reserved(coordinator.reserve_managed_storage(&request, 1000).unwrap());
        coordinator
            .reconcile_managed_storage_claim(&request, 45, Some("test-instance:cleanup"), true)
            .unwrap();
        let pending = coordinator
            .mark_managed_storage_cleanup_pending(&request)
            .unwrap();
        assert_eq!(pending.state, ManagedStorageClaimState::CleanupPending);
        assert_eq!(pending.measured_bytes, Some(45));

        assert_eq!(
            conflict_code(
                VerifiedManagedStorageRemoval::after_complete_no_follow_removal(
                    request.repository.clone(),
                    request.filesystem_id.clone(),
                    request.claim_id.clone(),
                    request.root_identity.clone(),
                    None,
                    "receipt-absent-root".into(),
                    0,
                    true,
                )
                .unwrap_err()
            ),
            "managed_storage_removal_instance_missing"
        );
        assert_eq!(
            conflict_code(
                VerifiedManagedStorageRemoval::after_complete_no_follow_removal(
                    request.repository.clone(),
                    request.filesystem_id.clone(),
                    request.claim_id.clone(),
                    request.root_identity.clone(),
                    Some("test-instance:cleanup".into()),
                    "receipt-incomplete".into(),
                    1,
                    true,
                )
                .unwrap_err()
            ),
            "managed_storage_removal_unverified"
        );
        let wrong_root = VerifiedManagedStorageRemoval::after_complete_no_follow_removal(
            request.repository.clone(),
            request.filesystem_id.clone(),
            request.claim_id.clone(),
            "different-root".into(),
            Some("test-instance:original".into()),
            "receipt-wrong-root".into(),
            0,
            true,
        )
        .unwrap();
        assert_eq!(
            conflict_code(
                coordinator
                    .release_managed_storage_claim(&wrong_root)
                    .unwrap_err()
            ),
            "managed_storage_root_mismatch"
        );

        let receipt = VerifiedManagedStorageRemoval::after_complete_no_follow_removal(
            request.repository.clone(),
            request.filesystem_id.clone(),
            request.claim_id.clone(),
            request.root_identity.clone(),
            Some("test-instance:cleanup".into()),
            "receipt-complete".into(),
            0,
            true,
        )
        .unwrap();
        let saved_lineage = load_managed_storage_lineage(
            &coordinator.conn,
            &request.repository,
            &request.filesystem_id,
            &request.claim_id,
        )
        .unwrap()
        .unwrap();
        coordinator
            .conn
            .execute(
                "DELETE FROM managed_storage_claim_lineage
                 WHERE repository=?1 AND filesystem_id=?2 AND claim_id=?3",
                params![request.repository, request.filesystem_id, request.claim_id],
            )
            .unwrap();
        assert_eq!(
            conflict_code(
                coordinator
                    .release_managed_storage_claim(&receipt)
                    .unwrap_err()
            ),
            "managed_storage_lineage_missing"
        );
        assert_eq!(
            coordinator
                .managed_storage_claim(
                    &request.repository,
                    &request.filesystem_id,
                    &request.claim_id
                )
                .unwrap()
                .unwrap()
                .state,
            ManagedStorageClaimState::CleanupPending
        );
        coordinator
            .conn
            .execute(
                "INSERT INTO managed_storage_claim_lineage
                 (repository,filesystem_id,claim_id,predecessor_claim_id,baseline_bytes,
                  root_instance_id,superseded_by_claim_id,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    request.repository,
                    request.filesystem_id,
                    request.claim_id,
                    saved_lineage.predecessor_claim_id,
                    managed_storage_i64("baseline_bytes", saved_lineage.baseline_bytes).unwrap(),
                    saved_lineage.root_instance_id,
                    saved_lineage.superseded_by_claim_id,
                    now_ms(),
                ],
            )
            .unwrap();
        let released = coordinator.release_managed_storage_claim(&receipt).unwrap();
        assert_eq!(released.state, ManagedStorageClaimState::Released);
        assert_eq!(released.measured_bytes, Some(45));
        assert_eq!(
            coordinator.release_managed_storage_claim(&receipt).unwrap(),
            released
        );
        assert_eq!(
            existing_claim(coordinator.reserve_managed_storage(&request, 0).unwrap()).state,
            ManagedStorageClaimState::Released
        );

        let next = managed_claim("after-cleanup", 100);
        assert_eq!(
            newly_reserved(coordinator.reserve_managed_storage(&next, 100).unwrap()).state,
            ManagedStorageClaimState::Reserved
        );
    }

    #[test]
    fn charged_claim_retry_is_not_a_new_materialization_authorization() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host.db");
        let request = managed_claim("charged-retry", 20);
        {
            let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
            coordinator
                .set_managed_storage_policy(&managed_policy("aethyme-main", "device-42", 100, 0))
                .unwrap();
            newly_reserved(coordinator.reserve_managed_storage(&request, 1000).unwrap());
            coordinator
                .reconcile_managed_storage_claim(
                    &request,
                    18,
                    Some("test-instance:charged-retry"),
                    true,
                )
                .unwrap();
        }

        let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
        assert!(matches!(
            coordinator.reserve_managed_storage(&request, 1000).unwrap(),
            ManagedStorageReservationResult::Existing(ManagedStorageClaim {
                state: ManagedStorageClaimState::Charged,
                ..
            })
        ));
        let recovered = coordinator
            .managed_storage_claim(
                &request.repository,
                &request.filesystem_id,
                &request.claim_id,
            )
            .unwrap()
            .unwrap();
        assert_eq!(recovered.state, ManagedStorageClaimState::Charged);
        assert_eq!(recovered.measured_bytes, Some(18));
    }

    #[test]
    fn concurrent_managed_storage_reservations_serialize_the_cap_check() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host.db");
        {
            let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
            coordinator
                .set_managed_storage_policy(&managed_policy("aethyme-main", "device-42", 100, 0))
                .unwrap();
        }

        let barrier = Arc::new(Barrier::new(2));
        let handles = ["parallel-1", "parallel-2"].map(|claim_id| {
            let db = db.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
                let request = managed_claim(claim_id, 60);
                barrier.wait();
                matches!(
                    coordinator.reserve_managed_storage(&request, 1000),
                    Ok(ManagedStorageReservationResult::Created(_))
                )
            })
        });
        let successes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|success| *success)
            .count();
        assert_eq!(successes, 1);
    }

    #[test]
    fn managed_storage_claim_ids_bind_exact_request_contents() {
        let temp = tempfile::tempdir().unwrap();
        let mut coordinator = HostResourceCoordinator::open(&temp.path().join("host.db")).unwrap();
        coordinator
            .set_managed_storage_policy(&managed_policy("aethyme-main", "device-42", 100, 0))
            .unwrap();
        let original = managed_claim("same-id", 25);
        newly_reserved(
            coordinator
                .reserve_managed_storage(&original, 1000)
                .unwrap(),
        );
        let changed = managed_claim("same-id", 26);
        assert!(matches!(
            coordinator.reserve_managed_storage(&changed, 1000),
            Err(HostResourceError::IdempotencyMismatch(_))
        ));
    }

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
