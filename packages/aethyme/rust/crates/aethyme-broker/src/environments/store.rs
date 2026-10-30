//! Durable environment records (#1222).
//!
//! Environments live in their own host-scoped database,
//! `<host state>/environments.db`, rather than as new tables in
//! `host-resources.db`. The host-resource store is shared by every installed
//! binary on the host and refuses unknown schema versions, so bumping it from a
//! branch build would lock the installed `aethyme` out of all leases. A
//! separate file keeps that store's schema untouched; environments reference
//! leases by id.
//!
//! Phases are validated in code, not by a SQL `CHECK`, so adding a phase later
//! is not a table rebuild. Every phase change is a compare-and-swap on
//! `phase_generation` and appends one `environment_transitions` row in the same
//! transaction, so history and state cannot disagree.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use super::compile::{CompiledEnvironment, EnvironmentOwner};
use crate::managed_services::ManagedServicePhase;

pub const ENVIRONMENTS_DB: &str = "environments.db";
/// v2 (#1211): `environments.main_root`, so destroy can export the recorded
/// commit after the worktree is gone.
pub const STORE_SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS environments (
  env_id TEXT PRIMARY KEY,
  repository TEXT NOT NULL,
  owner_kind TEXT NOT NULL,
  owner_ref TEXT NOT NULL,
  worktree_fingerprint TEXT NOT NULL,
  worktree_root TEXT,
  main_root TEXT,
  session_id INTEGER,
  profile TEXT NOT NULL,
  profile_digest TEXT NOT NULL,
  spec_snapshot_json TEXT NOT NULL,
  namespace TEXT UNIQUE,
  phase TEXT NOT NULL,
  phase_generation INTEGER NOT NULL,
  reservation_lease_id TEXT,
  capacity_lease_id TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  last_activity_at INTEGER,
  pinned_until INTEGER,
  pin_reason TEXT,
  parked_at INTEGER,
  last_error_json TEXT
);
CREATE INDEX IF NOT EXISTS environments_repository ON environments(repository);
CREATE TABLE IF NOT EXISTS environment_components (
  env_id TEXT NOT NULL REFERENCES environments(env_id) ON DELETE CASCADE,
  component TEXT NOT NULL,
  runtime TEXT NOT NULL,
  phase TEXT NOT NULL,
  identity_json TEXT,
  receipt_json TEXT,
  attempt INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (env_id, component)
);
CREATE TABLE IF NOT EXISTS environment_transitions (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  env_id TEXT NOT NULL,
  from_phase TEXT,
  to_phase TEXT NOT NULL,
  reason TEXT NOT NULL,
  actor TEXT NOT NULL,
  at INTEGER NOT NULL,
  detail_json TEXT
);
CREATE INDEX IF NOT EXISTS environment_transitions_env ON environment_transitions(env_id, seq);
CREATE TABLE IF NOT EXISTS environment_samples (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  env_id TEXT NOT NULL,
  repository TEXT NOT NULL,
  profile TEXT NOT NULL,
  at INTEGER NOT NULL,
  bytes INTEGER,
  cpu_pct REAL,
  method TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS environment_samples_profile ON environment_samples(repository, profile, seq);
";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentPhase {
    /// Recorded; nothing outside the store exists yet.
    Planned,
    Provisioning,
    Starting,
    Active,
    Stopping,
    /// Stopped with capacity released; reservation and provisioned data kept.
    Parked,
    Waking,
    Destroying,
    Destroyed,
    /// Provisioning or a first start failed; destroy or retry.
    Failed,
    /// A destroy hook failed; leases stay held until it succeeds.
    DestroyFailed,
    /// Ownership could not be proven; nothing is signalled until reconciled.
    Quarantined,
}

impl EnvironmentPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Provisioning => "provisioning",
            Self::Starting => "starting",
            Self::Active => "active",
            Self::Stopping => "stopping",
            Self::Parked => "parked",
            Self::Waking => "waking",
            Self::Destroying => "destroying",
            Self::Destroyed => "destroyed",
            Self::Failed => "failed",
            Self::DestroyFailed => "destroy_failed",
            Self::Quarantined => "quarantined",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "planned" => Self::Planned,
            "provisioning" => Self::Provisioning,
            "starting" => Self::Starting,
            "active" => Self::Active,
            "stopping" => Self::Stopping,
            "parked" => Self::Parked,
            "waking" => Self::Waking,
            "destroying" => Self::Destroying,
            "destroyed" => Self::Destroyed,
            "failed" => Self::Failed,
            "destroy_failed" => Self::DestroyFailed,
            "quarantined" => Self::Quarantined,
            _ => return None,
        })
    }

    /// Topology only: whether `self` may directly follow `previous`. The
    /// caller still proves the evidence for the transition.
    pub fn may_follow(self, previous: Self) -> bool {
        use EnvironmentPhase::*;
        matches!(
            (previous, self),
            (Planned, Provisioning | Destroying | Destroyed)
                | (Provisioning, Starting | Parked | Failed | Destroying)
                | (Starting, Active | Stopping | Failed)
                | (Active, Stopping | Destroying | Quarantined)
                | (Stopping, Parked | Destroying | Quarantined)
                | (Parked, Waking | Destroying)
                // A failed wake rolls back to parked; it never stays half-up.
                | (Waking, Active | Parked | Quarantined)
                | (Failed, Provisioning | Destroying)
                | (Destroying, Destroyed | DestroyFailed)
                | (DestroyFailed, Destroying)
                | (Quarantined, Active | Parked | Destroying)
                // Re-creating a destroyed environment reuses its stable id.
                | (Destroyed, Planned)
        )
    }

    /// Whether anything outside the store may exist for this environment.
    /// The worktree must not be removed while this is true (#1211).
    pub fn retains_worktree(self) -> bool {
        !matches!(self, Self::Planned | Self::Destroyed)
    }

    /// Whether the environment holds (or is acquiring) pool capacity.
    pub fn holds_capacity(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Active | Self::Stopping | Self::Waking
        )
    }

    /// Whether a host supervisor must be running for this phase to make
    /// progress or be watched.
    pub fn needs_supervisor(self) -> bool {
        matches!(
            self,
            Self::Provisioning
                | Self::Starting
                | Self::Active
                | Self::Stopping
                | Self::Waking
                | Self::Destroying
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvironmentRecord {
    pub env_id: String,
    pub repository: String,
    pub owner: EnvironmentOwner,
    pub worktree_fingerprint: String,
    pub worktree_root: Option<String>,
    /// The repository's main checkout, used to export the recorded commit
    /// when the worktree is gone (#1211).
    pub main_root: Option<String>,
    pub session_id: Option<i64>,
    pub profile: String,
    pub profile_digest: String,
    #[serde(skip)]
    pub snapshot: CompiledEnvironment,
    pub namespace: Option<String>,
    pub phase: EnvironmentPhase,
    pub phase_generation: i64,
    pub reservation_lease_id: Option<String>,
    pub capacity_lease_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_activity_at: Option<i64>,
    pub pinned_until: Option<i64>,
    pub pin_reason: Option<String>,
    pub parked_at: Option<i64>,
    pub last_error: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentRecord {
    pub component: String,
    pub runtime: String,
    pub phase: ManagedServicePhase,
    pub identity: Option<serde_json::Value>,
    pub receipt: Option<serde_json::Value>,
    pub attempt: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Transition {
    pub seq: i64,
    pub env_id: String,
    pub from: Option<EnvironmentPhase>,
    pub to: EnvironmentPhase,
    pub reason: String,
    pub actor: String,
    pub at: i64,
    pub detail: Option<serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("environments database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("environments database {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "environments database schema {found} is newer than this binary supports ({supported}); upgrade aethyme"
    )]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("host state directory is unavailable; set AETHYME_HOST_STATE_DIR or HOME")]
    StateDirectoryUnavailable,
    #[error("environment {0} does not exist")]
    NotFound(String),
    #[error("environment {env_id} moved to generation {actual}; expected {expected}")]
    Conflict {
        env_id: String,
        expected: i64,
        actual: i64,
    },
    #[error("environment {env_id} cannot go from {from} to {to}")]
    IllegalTransition {
        env_id: String,
        from: &'static str,
        to: &'static str,
    },
    #[error("component {component} of {env_id} cannot go from {from} to {to}")]
    IllegalComponentTransition {
        env_id: String,
        component: String,
        from: &'static str,
        to: &'static str,
    },
    #[error("namespace {0} is already recorded for another environment")]
    NamespaceTaken(String),
    #[error("environment prefix {0:?} matches more than one environment")]
    Ambiguous(String),
    #[error("stored record is corrupt: {0}")]
    Corrupt(String),
}

pub fn default_store_path() -> Result<PathBuf, StoreError> {
    crate::host_state::default_host_state_dir()
        .map(|directory| directory.join(ENVIRONMENTS_DB))
        .ok_or(StoreError::StateDirectoryUnavailable)
}

pub struct EnvironmentStore {
    conn: Connection,
}

/// `env-` plus 12 lowercase hex: the only shape an id has, checked before an
/// id ever becomes part of a path.
pub fn valid_env_id(value: &str) -> bool {
    value.len() == 16
        && value.starts_with("env-")
        && value[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl EnvironmentStore {
    /// Open writable, creating the database and its directory if needed.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.into(),
                source,
            })?;
            crate::host_state::protect_host_state_path(parent, true).map_err(|source| {
                StoreError::Io {
                    path: parent.into(),
                    source,
                }
            })?;
        }
        let conn = Connection::open(path)?;
        crate::host_state::protect_host_state_path(path, false).map_err(|source| {
            StoreError::Io {
                path: path.into(),
                source,
            }
        })?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Self::check_version(&conn)?;
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        // v1 -> v2: additive column. Older binaries ignore it.
        let has_main_root = conn
            .prepare("SELECT 1 FROM pragma_table_info('environments') WHERE name = 'main_root'")?
            .exists([])?;
        if !has_main_root {
            conn.execute_batch("ALTER TABLE environments ADD COLUMN main_root TEXT")?;
        }
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value
             WHERE CAST(meta.value AS INTEGER) < CAST(excluded.value AS INTEGER)",
            params![STORE_SCHEMA_VERSION.to_string()],
        )?;
        Ok(Self { conn })
    }

    pub fn open_default() -> Result<Self, StoreError> {
        Self::open(&default_store_path()?)
    }

    /// Open for reading without creating anything. `None` when no
    /// environment has ever been recorded on this host.
    pub fn open_read_only(path: &Path) -> Result<Option<Self>, StoreError> {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Io {
                    path: path.into(),
                    source,
                });
            }
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(StoreError::Io {
                    path: path.into(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "environments database path is not a regular file",
                    ),
                });
            }
        }
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Self::check_version(&conn)?;
        let current = conn
            .prepare("SELECT 1 FROM pragma_table_info('environments') WHERE name = 'main_root'")?
            .exists([])?;
        if !current {
            // An existing store from an older binary: bring it forward once
            // (additive only), then read. A missing store is still never
            // created by a read.
            drop(conn);
            drop(Self::open(path)?);
            return Self::open_read_only(path);
        }
        Ok(Some(Self { conn }))
    }

    pub fn open_default_read_only() -> Result<Option<Self>, StoreError> {
        Self::open_read_only(&default_store_path()?)
    }

    fn check_version(conn: &Connection) -> Result<(), StoreError> {
        let has_meta: bool = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)?;
        if !has_meta {
            return Ok(());
        }
        let found: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(found) = found {
            let found: i64 = found
                .parse()
                .map_err(|_| StoreError::Corrupt(format!("schema_version {found:?}")))?;
            if found > STORE_SCHEMA_VERSION {
                return Err(StoreError::SchemaTooNew {
                    found,
                    supported: STORE_SCHEMA_VERSION,
                });
            }
        }
        Ok(())
    }

    /// Record an environment, or return the existing record for the same id.
    ///
    /// An existing live record is returned unchanged even if the snapshot
    /// differs: replacing the commands of something that may be running is a
    /// reconfiguration, which the lifecycle layer performs explicitly. A
    /// `destroyed` record is re-planned with the new snapshot.
    pub fn register(
        &mut self,
        snapshot: &CompiledEnvironment,
        worktree_root: Option<&Path>,
        session_id: Option<i64>,
    ) -> Result<EnvironmentRecord, StoreError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = load_record(&tx, &snapshot.env_id)?;
        let snapshot_json = serde_json::to_string(snapshot)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        let root = worktree_root.map(|root| root.to_string_lossy().into_owned());
        match existing {
            Some(record) if record.phase != EnvironmentPhase::Destroyed => {
                tx.commit()?;
                return Ok(record);
            }
            Some(record) => {
                tx.execute(
                    "UPDATE environments SET profile_digest = ?2, spec_snapshot_json = ?3,
                       worktree_root = ?4, session_id = ?5, phase = 'planned',
                       phase_generation = phase_generation + 1, namespace = NULL,
                       reservation_lease_id = NULL, capacity_lease_id = NULL,
                       parked_at = NULL, last_error_json = NULL, updated_at = ?6
                     WHERE env_id = ?1",
                    params![
                        snapshot.env_id,
                        snapshot.digest,
                        snapshot_json,
                        root,
                        session_id,
                        now
                    ],
                )?;
                tx.execute(
                    "DELETE FROM environment_components WHERE env_id = ?1",
                    params![snapshot.env_id],
                )?;
                insert_transition(
                    &tx,
                    &snapshot.env_id,
                    Some(record.phase),
                    EnvironmentPhase::Planned,
                    "re-created",
                    "register",
                    now,
                    None,
                )?;
            }
            None => {
                let (owner_kind, owner_ref) = owner_columns(&snapshot.owner);
                tx.execute(
                    "INSERT INTO environments (env_id, repository, owner_kind, owner_ref,
                       worktree_fingerprint, worktree_root, session_id, profile, profile_digest,
                       spec_snapshot_json, phase, phase_generation, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'planned', 0, ?11, ?11)",
                    params![
                        snapshot.env_id,
                        snapshot.repository,
                        owner_kind,
                        owner_ref,
                        snapshot.worktree_fingerprint,
                        root,
                        session_id,
                        snapshot.profile,
                        snapshot.digest,
                        snapshot_json,
                        now
                    ],
                )?;
                insert_transition(
                    &tx,
                    &snapshot.env_id,
                    None,
                    EnvironmentPhase::Planned,
                    "registered",
                    "register",
                    now,
                    None,
                )?;
            }
        }
        for component in &snapshot.components {
            tx.execute(
                "INSERT INTO environment_components (env_id, component, runtime, phase, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(env_id, component) DO NOTHING",
                params![
                    snapshot.env_id,
                    component.name,
                    component.runtime.as_str(),
                    ManagedServicePhase::Intent.as_str(),
                    now
                ],
            )?;
        }
        let record = load_record(&tx, &snapshot.env_id)?
            .ok_or_else(|| StoreError::NotFound(snapshot.env_id.clone()))?;
        tx.commit()?;
        Ok(record)
    }

    pub fn get(&self, env_id: &str) -> Result<Option<EnvironmentRecord>, StoreError> {
        load_record(&self.conn, env_id)
    }

    /// Resolve a full id or a prefix of at least 8 characters.
    pub fn resolve(&self, id_or_prefix: &str) -> Result<EnvironmentRecord, StoreError> {
        if valid_env_id(id_or_prefix) {
            return self
                .get(id_or_prefix)?
                .ok_or_else(|| StoreError::NotFound(id_or_prefix.into()));
        }
        if id_or_prefix.len() < 8
            || !id_or_prefix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(StoreError::NotFound(id_or_prefix.into()));
        }
        let mut statement = self
            .conn
            .prepare("SELECT env_id FROM environments WHERE env_id LIKE ?1 || '%' LIMIT 2")?;
        let ids: Vec<String> = statement
            .query_map(params![id_or_prefix], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        match ids.as_slice() {
            [one] => self
                .get(one)?
                .ok_or_else(|| StoreError::NotFound(one.clone())),
            [] => Err(StoreError::NotFound(id_or_prefix.into())),
            _ => Err(StoreError::Ambiguous(id_or_prefix.into())),
        }
    }

    /// Environments, newest first. `repository` narrows to one repository.
    pub fn list(&self, repository: Option<&str>) -> Result<Vec<EnvironmentRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT env_id FROM environments WHERE ?1 IS NULL OR repository = ?1
             ORDER BY updated_at DESC, env_id",
        )?;
        let ids: Vec<String> = statement
            .query_map(params![repository], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        ids.iter()
            .filter_map(|id| load_record(&self.conn, id).transpose())
            .collect()
    }

    /// Whether any recorded environment needs a running supervisor.
    pub fn any_needs_supervisor(&self) -> Result<bool, StoreError> {
        Ok(self
            .list(None)?
            .iter()
            .any(|record| record.phase.needs_supervisor()))
    }

    pub fn components(&self, env_id: &str) -> Result<Vec<ComponentRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT component, runtime, phase, identity_json, receipt_json, attempt, updated_at
             FROM environment_components WHERE env_id = ?1",
        )?;
        let rows = statement.query_map(params![env_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?;
        let mut components = Vec::new();
        for row in rows {
            let (component, runtime, phase, identity, receipt, attempt, updated_at) = row?;
            components.push(ComponentRecord {
                component,
                runtime,
                phase: parse_service_phase(&phase)?,
                identity: parse_json(identity)?,
                receipt: parse_json(receipt)?,
                attempt,
                updated_at,
            });
        }
        // Report in start order, which is the snapshot's order.
        if let Some(record) = self.get(env_id)? {
            let order: Vec<&str> = record
                .snapshot
                .components
                .iter()
                .map(|c| c.name.as_str())
                .collect();
            components.sort_by_key(|c| {
                order
                    .iter()
                    .position(|name| *name == c.component)
                    .unwrap_or(usize::MAX)
            });
        }
        Ok(components)
    }

    pub fn transitions(&self, env_id: &str, limit: usize) -> Result<Vec<Transition>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT seq, env_id, from_phase, to_phase, reason, actor, at, detail_json
             FROM environment_transitions WHERE env_id = ?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![env_id, limit as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?;
        let mut transitions = Vec::new();
        for row in rows {
            let (seq, env_id, from, to, reason, actor, at, detail) = row?;
            transitions.push(Transition {
                seq,
                env_id,
                from: from.as_deref().map(parse_phase).transpose()?,
                to: parse_phase(&to)?,
                reason,
                actor,
                at,
                detail: parse_json(detail)?,
            });
        }
        transitions.reverse();
        Ok(transitions)
    }

    /// Compare-and-swap a phase change and record it. `expected_generation`
    /// is the generation the caller read; a concurrent change makes this
    /// fail with `Conflict` instead of overwriting it.
    #[allow(clippy::too_many_arguments)]
    pub fn transition(
        &mut self,
        env_id: &str,
        expected_generation: i64,
        to: EnvironmentPhase,
        reason: &str,
        actor: &str,
        detail: Option<serde_json::Value>,
    ) -> Result<EnvironmentRecord, StoreError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record =
            load_record(&tx, env_id)?.ok_or_else(|| StoreError::NotFound(env_id.into()))?;
        if record.phase_generation != expected_generation {
            return Err(StoreError::Conflict {
                env_id: env_id.into(),
                expected: expected_generation,
                actual: record.phase_generation,
            });
        }
        if !to.may_follow(record.phase) {
            return Err(StoreError::IllegalTransition {
                env_id: env_id.into(),
                from: record.phase.as_str(),
                to: to.as_str(),
            });
        }
        let parked_at = (to == EnvironmentPhase::Parked).then_some(now);
        tx.execute(
            "UPDATE environments SET phase = ?2, phase_generation = phase_generation + 1,
               updated_at = ?3,
               parked_at = CASE WHEN ?2 = 'parked' THEN ?4 ELSE NULL END
             WHERE env_id = ?1",
            params![env_id, to.as_str(), now, parked_at],
        )?;
        insert_transition(
            &tx,
            env_id,
            Some(record.phase),
            to,
            reason,
            actor,
            now,
            detail.as_ref(),
        )?;
        let updated =
            load_record(&tx, env_id)?.ok_or_else(|| StoreError::NotFound(env_id.into()))?;
        tx.commit()?;
        Ok(updated)
    }

    /// Record the namespace a reservation lease granted. Namespaces are
    /// unique across environments; a collision is an error, not a reuse.
    pub fn set_namespace(&mut self, env_id: &str, namespace: &str) -> Result<(), StoreError> {
        match self.conn.execute(
            "UPDATE environments SET namespace = ?2, updated_at = ?3 WHERE env_id = ?1",
            params![env_id, namespace, now_ms()],
        ) {
            Ok(0) => Err(StoreError::NotFound(env_id.into())),
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(StoreError::NamespaceTaken(namespace.into()))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Forget a component's instance after its rollback destroy succeeded:
    /// back to `intent` with no identity or receipt, so the next `up`
    /// provisions it again. Only for confirmed rollbacks; it bypasses phase
    /// topology on purpose.
    pub fn reset_component(&mut self, env_id: &str, component: &str) -> Result<(), StoreError> {
        let changed = self.conn.execute(
            "UPDATE environment_components SET phase = ?3, identity_json = NULL,
               receipt_json = NULL, updated_at = ?4
             WHERE env_id = ?1 AND component = ?2",
            params![
                env_id,
                component,
                ManagedServicePhase::Intent.as_str(),
                now_ms()
            ],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("{env_id}/{component}")));
        }
        Ok(())
    }

    /// Swap the recorded snapshot and source of a stopped environment, keeping
    /// its id, reservation, namespace and ports (#1217 hold-and-swap). Only a
    /// `parked` or `planned` environment can be swapped: nothing may be
    /// running from the old commands.
    pub fn replace_snapshot(
        &mut self,
        env_id: &str,
        snapshot: &CompiledEnvironment,
        worktree_root: &Path,
    ) -> Result<EnvironmentRecord, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record =
            load_record(&tx, env_id)?.ok_or_else(|| StoreError::NotFound(env_id.into()))?;
        if !matches!(
            record.phase,
            EnvironmentPhase::Parked | EnvironmentPhase::Planned
        ) {
            return Err(StoreError::IllegalTransition {
                env_id: env_id.into(),
                from: record.phase.as_str(),
                to: "snapshot swap",
            });
        }
        let snapshot_json = serde_json::to_string(snapshot)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        tx.execute(
            "UPDATE environments SET profile_digest = ?2, spec_snapshot_json = ?3,
               worktree_root = ?4, updated_at = ?5 WHERE env_id = ?1",
            params![
                env_id,
                snapshot.digest,
                snapshot_json,
                worktree_root.to_string_lossy(),
                now_ms()
            ],
        )?;
        for component in &snapshot.components {
            tx.execute(
                "INSERT INTO environment_components (env_id, component, runtime, phase, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(env_id, component) DO NOTHING",
                params![
                    env_id,
                    component.name,
                    component.runtime.as_str(),
                    ManagedServicePhase::Intent.as_str(),
                    now_ms()
                ],
            )?;
        }
        let updated =
            load_record(&tx, env_id)?.ok_or_else(|| StoreError::NotFound(env_id.into()))?;
        tx.commit()?;
        Ok(updated)
    }

    /// Record one resource sample (#1218) and keep at most
    /// `keep` samples per (repository, profile).
    pub fn record_sample(
        &mut self,
        record: &EnvironmentRecord,
        sample: &super::metrics::Sample,
        keep: usize,
    ) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO environment_samples (env_id, repository, profile, at, bytes, cpu_pct, method)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.env_id,
                record.repository,
                record.profile,
                sample.sampled_at,
                sample.bytes.map(|b| b as i64),
                sample.cpu_pct,
                sample.method
            ],
        )?;
        tx.execute(
            "DELETE FROM environment_samples WHERE repository = ?1 AND profile = ?2 AND seq NOT IN (
               SELECT seq FROM environment_samples WHERE repository = ?1 AND profile = ?2
               ORDER BY seq DESC LIMIT ?3)",
            params![record.repository, record.profile, keep as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The newest sample of an environment.
    pub fn latest_sample(
        &self,
        env_id: &str,
    ) -> Result<Option<super::metrics::Sample>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT bytes, cpu_pct, method, at FROM environment_samples
                 WHERE env_id = ?1 ORDER BY seq DESC LIMIT 1",
                params![env_id],
                |row| {
                    Ok(super::metrics::Sample {
                        bytes: row.get::<_, Option<i64>>(0)?.map(|b| b as u64),
                        cpu_pct: row.get(1)?,
                        method: row.get(2)?,
                        sampled_at: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// Measured byte samples per profile of one repository (unknown
    /// measurements excluded).
    pub fn footprints(
        &self,
        repository: &str,
    ) -> Result<std::collections::BTreeMap<String, Vec<u64>>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT profile, bytes FROM environment_samples
             WHERE repository = ?1 AND bytes IS NOT NULL",
        )?;
        let rows = statement.query_map(params![repository], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut history: std::collections::BTreeMap<String, Vec<u64>> = Default::default();
        for row in rows {
            let (profile, bytes) = row?;
            history.entry(profile).or_default().push(bytes as u64);
        }
        Ok(history)
    }

    pub fn set_pin(
        &mut self,
        env_id: &str,
        until: Option<i64>,
        reason: Option<&str>,
    ) -> Result<(), StoreError> {
        let changed = self.conn.execute(
            "UPDATE environments SET pinned_until = ?2, pin_reason = ?3, updated_at = ?4
             WHERE env_id = ?1",
            params![env_id, until, reason, now_ms()],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(env_id.into()));
        }
        Ok(())
    }

    /// Record an event that is not a phase change (a pin, an idle park's
    /// reason) in the same history, as a transition from the current phase
    /// to itself.
    pub fn note(
        &mut self,
        env_id: &str,
        reason: &str,
        actor: &str,
        detail: Option<&serde_json::Value>,
    ) -> Result<(), StoreError> {
        let record = self
            .get(env_id)?
            .ok_or_else(|| StoreError::NotFound(env_id.into()))?;
        insert_transition(
            &self.conn,
            env_id,
            Some(record.phase),
            record.phase,
            reason,
            actor,
            now_ms(),
            detail,
        )
    }

    pub fn set_main_root(&mut self, env_id: &str, main_root: &Path) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE environments SET main_root = ?2 WHERE env_id = ?1",
            params![env_id, main_root.to_string_lossy()],
        )?;
        Ok(())
    }

    pub fn set_leases(
        &mut self,
        env_id: &str,
        reservation: Option<&str>,
        capacity: Option<&str>,
    ) -> Result<(), StoreError> {
        let changed = self.conn.execute(
            "UPDATE environments SET reservation_lease_id = ?2, capacity_lease_id = ?3,
               updated_at = ?4 WHERE env_id = ?1",
            params![env_id, reservation, capacity, now_ms()],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(env_id.into()));
        }
        Ok(())
    }

    pub fn set_error(
        &mut self,
        env_id: &str,
        error: Option<&serde_json::Value>,
    ) -> Result<(), StoreError> {
        let json = error.map(serde_json::Value::to_string);
        self.conn.execute(
            "UPDATE environments SET last_error_json = ?2, updated_at = ?3 WHERE env_id = ?1",
            params![env_id, json, now_ms()],
        )?;
        Ok(())
    }

    pub fn record_activity(&mut self, env_id: &str, at: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE environments SET last_activity_at = max(coalesce(last_activity_at, 0), ?2)
             WHERE env_id = ?1",
            params![env_id, at],
        )?;
        Ok(())
    }

    /// Move one component's service phase, write-ahead: callers record the
    /// pending phase before the external operation. `settled -> intent`
    /// starts a new instance (a wake after a park) and increments `attempt`.
    pub fn set_component(
        &mut self,
        env_id: &str,
        component: &str,
        to: ManagedServicePhase,
        identity: Option<&serde_json::Value>,
        receipt: Option<&serde_json::Value>,
    ) -> Result<ComponentRecord, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(String, i64)> = tx
            .query_row(
                "SELECT phase, attempt FROM environment_components
                 WHERE env_id = ?1 AND component = ?2",
                params![env_id, component],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((phase, attempt)) = current else {
            return Err(StoreError::NotFound(format!("{env_id}/{component}")));
        };
        let from = parse_service_phase(&phase)?;
        let new_instance =
            from == ManagedServicePhase::Settled && to == ManagedServicePhase::Intent;
        if !(to.may_follow(from) || new_instance) {
            return Err(StoreError::IllegalComponentTransition {
                env_id: env_id.into(),
                component: component.into(),
                from: from.as_str(),
                to: to.as_str(),
            });
        }
        let attempt = if new_instance { attempt + 1 } else { attempt };
        tx.execute(
            "UPDATE environment_components SET phase = ?3, attempt = ?4, updated_at = ?5,
               identity_json = coalesce(?6, CASE WHEN ?7 THEN NULL ELSE identity_json END),
               receipt_json = coalesce(?8, receipt_json)
             WHERE env_id = ?1 AND component = ?2",
            params![
                env_id,
                component,
                to.as_str(),
                attempt,
                now_ms(),
                identity.map(serde_json::Value::to_string),
                new_instance,
                receipt.map(serde_json::Value::to_string),
            ],
        )?;
        tx.commit()?;
        self.components(env_id)?
            .into_iter()
            .find(|c| c.component == component)
            .ok_or_else(|| StoreError::NotFound(format!("{env_id}/{component}")))
    }
}

fn owner_columns(owner: &EnvironmentOwner) -> (&'static str, String) {
    match owner {
        EnvironmentOwner::Worktree { fingerprint } => ("worktree", fingerprint.clone()),
        EnvironmentOwner::Ref { name } => ("ref", name.clone()),
    }
}

fn parse_phase(value: &str) -> Result<EnvironmentPhase, StoreError> {
    EnvironmentPhase::parse(value).ok_or_else(|| StoreError::Corrupt(format!("phase {value:?}")))
}

fn parse_service_phase(value: &str) -> Result<ManagedServicePhase, StoreError> {
    use ManagedServicePhase::*;
    [
        Intent,
        AdmissionPending,
        Admitted,
        CreatePending,
        Created,
        StartPending,
        Running,
        RemovalPending,
        Removed,
        Settled,
    ]
    .into_iter()
    .find(|phase| phase.as_str() == value)
    .ok_or_else(|| StoreError::Corrupt(format!("component phase {value:?}")))
}

fn parse_json(value: Option<String>) -> Result<Option<serde_json::Value>, StoreError> {
    value
        .map(|text| serde_json::from_str(&text))
        .transpose()
        .map_err(|error| StoreError::Corrupt(error.to_string()))
}

#[allow(clippy::too_many_arguments)]
fn insert_transition(
    conn: &Connection,
    env_id: &str,
    from: Option<EnvironmentPhase>,
    to: EnvironmentPhase,
    reason: &str,
    actor: &str,
    at: i64,
    detail: Option<&serde_json::Value>,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO environment_transitions (env_id, from_phase, to_phase, reason, actor, at, detail_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            env_id,
            from.map(EnvironmentPhase::as_str),
            to.as_str(),
            reason,
            actor,
            at,
            detail.map(serde_json::Value::to_string)
        ],
    )?;
    Ok(())
}

fn load_record(conn: &Connection, env_id: &str) -> Result<Option<EnvironmentRecord>, StoreError> {
    let row = conn
        .query_row(
            "SELECT env_id, repository, owner_kind, owner_ref, worktree_fingerprint,
               worktree_root, session_id, profile, profile_digest, spec_snapshot_json,
               namespace, phase, phase_generation, reservation_lease_id, capacity_lease_id,
               created_at, updated_at, last_activity_at, pinned_until, pin_reason, parked_at,
               last_error_json, main_root
             FROM environments WHERE env_id = ?1",
            params![env_id],
            |row| {
                Ok((
                    (
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, Option<String>>(10)?,
                    ),
                    (
                        row.get::<_, String>(11)?,
                        row.get::<_, i64>(12)?,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, Option<String>>(14)?,
                        row.get::<_, i64>(15)?,
                        row.get::<_, i64>(16)?,
                        row.get::<_, Option<i64>>(17)?,
                        row.get::<_, Option<i64>>(18)?,
                        row.get::<_, Option<String>>(19)?,
                        row.get::<_, Option<i64>>(20)?,
                        row.get::<_, Option<String>>(21)?,
                        row.get::<_, Option<String>>(22)?,
                    ),
                ))
            },
        )
        .optional()?;
    let Some((
        (
            env_id,
            repository,
            owner_kind,
            owner_ref,
            worktree_fingerprint,
            worktree_root,
            session_id,
            profile,
            profile_digest,
            snapshot_json,
            namespace,
        ),
        (
            phase,
            phase_generation,
            reservation_lease_id,
            capacity_lease_id,
            created_at,
            updated_at,
            last_activity_at,
            pinned_until,
            pin_reason,
            parked_at,
            last_error,
            main_root,
        ),
    )) = row
    else {
        return Ok(None);
    };
    let owner = match owner_kind.as_str() {
        "worktree" => EnvironmentOwner::Worktree {
            fingerprint: owner_ref,
        },
        "ref" => EnvironmentOwner::Ref { name: owner_ref },
        other => return Err(StoreError::Corrupt(format!("owner kind {other:?}"))),
    };
    let snapshot: CompiledEnvironment = serde_json::from_str(&snapshot_json)
        .map_err(|error| StoreError::Corrupt(format!("snapshot of {env_id}: {error}")))?;
    Ok(Some(EnvironmentRecord {
        env_id,
        repository,
        owner,
        worktree_fingerprint,
        worktree_root,
        main_root,
        session_id,
        profile,
        profile_digest,
        snapshot,
        namespace,
        phase: parse_phase(&phase)?,
        phase_generation,
        reservation_lease_id,
        capacity_lease_id,
        created_at,
        updated_at,
        last_activity_at,
        pinned_until,
        pin_reason,
        parked_at,
        last_error: parse_json(last_error)?,
    }))
}
