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
"#;

#[derive(Debug, thiserror::Error)]
pub enum HostResourceError {
    #[error("host resource state at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("host resource database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid host resource request: {0}")]
    InvalidRequest(String),
    #[error("request {0:?} was already used with different contents")]
    IdempotencyMismatch(String),
    #[error("request {request_id:?} is {state}; use a new request id")]
    RequestNotActive { request_id: String, state: String },
    #[error("host resource bundle unavailable: {0}")]
    Conflict(String),
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
            protect_host_state_path(parent, true)?;
        }
        let conn = Connection::open(path)?;
        protect_host_state_path(path, false)?;
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
        quarantine_expired(&tx, now)?;
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
            return Err(HostResourceError::Conflict(
                conflicts
                    .iter()
                    .map(|c| format!("{}: {}", c.resource_key, c.reason))
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
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
        quarantine_expired(&tx, now)?;
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
        quarantine_expired(&tx, now)?;
        verify_ownership(&tx, lease_id, generation, token, false)?;
        tx.execute("UPDATE resource_leases SET state='released',released_at=?2,updated_at=?2 WHERE lease_id=?1 AND state!='released'", params![lease_id,now])?;
        let lease = load_lease(&tx, "lease_id", lease_id)?
            .ok_or_else(|| HostResourceError::LeaseNotFound(lease_id.into()))?;
        tx.commit()?;
        Ok(lease)
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
        quarantine_expired(&tx, now)?;
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

#[cfg(unix)]
fn protect_host_state_path(path: &Path, directory: bool) -> Result<(), HostResourceError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = if directory { 0o700 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|source| {
        HostResourceError::Io {
            path: path.into(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn protect_host_state_path(_path: &Path, _directory: bool) -> Result<(), HostResourceError> {
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
    if let Some(path) = std::env::var_os("AETHYME_HOST_STATE_DIR").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path).join("host-resources.db"));
    }
    if let Some(path) = std::env::var_os("XDG_STATE_HOME").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path).join("aethyme/host-resources.db"));
    }
    let home = PathBuf::from(
        std::env::var_os("HOME").ok_or(HostResourceError::StateDirectoryUnavailable)?,
    );
    let directory = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Aethyme")
    } else {
        home.join(".local/state/aethyme")
    };
    Ok(directory.join("host-resources.db"))
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
    kind: String,
    value: String,
    units: Option<u32>,
    limit: Option<u32>,
}

fn load_occupied(conn: &Connection) -> Result<Vec<Occupied>, HostResourceError> {
    let mut stmt=conn.prepare("SELECT a.lease_id,a.kind,a.value,a.units,a.capacity_limit FROM resource_allocations a JOIN resource_leases l ON l.lease_id=a.lease_id WHERE l.state IN ('active','quarantined') ORDER BY l.generation,a.resource_key")?;
    Ok(stmt
        .query_map([], |row| {
            Ok(Occupied {
                lease_id: row.get(0)?,
                kind: row.get(1)?,
                value: row.get(2)?,
                units: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                limit: row.get::<_, Option<i64>>(4)?.map(|v| v as u32),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
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
                        &requirement.key,
                        "capacity",
                        format!("{pool}:{units}/{limit}"),
                        &format!("pool has {used}/{limit} units allocated"),
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
                    conflicts.push(conflict(
                        &requirement.key,
                        "exclusive_key",
                        name.clone(),
                        "exclusive key is already allocated",
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
    key: &str,
    kind: &str,
    requested: String,
    reason: &str,
    owner: Option<String>,
) -> HostResourceConflict {
    HostResourceConflict {
        resource_key: key.into(),
        kind: kind.into(),
        requested,
        reason: reason.into(),
        owning_lease: owner,
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
            Err(HostResourceError::Conflict(_))
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
            Err(HostResourceError::Conflict(_))
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
                .filter(|result| matches!(result, Err(HostResourceError::Conflict(_))))
                .count(),
            1
        );
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
            Err(HostResourceError::Conflict(_))
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
