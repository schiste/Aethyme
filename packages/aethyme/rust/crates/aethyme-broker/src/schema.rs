//! Schema v1 for `.aethyme/broker.db`, plus migration machinery.
//!
//! Migration rules:
//! - `meta.schema_version` records the applied version; migrations run in
//!   order inside one transaction per version.
//! - Migrations are append-only: never edit an entry in [`MIGRATIONS`],
//!   only add new ones. Opening a database newer than this binary knows
//!   fails with [`BrokerError::SchemaTooNew`] rather than guessing.
//! - The `events` table is append-only by contract: the store exposes no
//!   update or delete for it, and each row carries its own
//!   `schema_version` ([`EVENTS_SCHEMA_VERSION`]) so old rows stay
//!   interpretable after the event contract evolves.

use rusqlite::Connection;

use crate::error::BrokerError;

/// Current database schema version (== `MIGRATIONS.len()`).
pub const SCHEMA_VERSION: i64 = 16;

/// Version stamped on every event row written by this binary.
pub const EVENTS_SCHEMA_VERSION: i64 = 1;

pub(crate) const MIGRATION_V1: &str = "
CREATE TABLE sessions (
    id               INTEGER PRIMARY KEY,
    worktree_path    TEXT NOT NULL,
    branch           TEXT NOT NULL,
    origin           TEXT NOT NULL CHECK (origin IN ('adopted', 'spawned')),
    status           TEXT NOT NULL DEFAULT 'active'
                     CHECK (status IN ('active', 'idle', 'stale', 'exited', 'cleaned')),
    task             TEXT,
    diff_base        TEXT,
    pid              INTEGER,
    command          TEXT,
    log_path         TEXT,
    exit_code        INTEGER,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    last_activity_at INTEGER NOT NULL
);

-- Attach-first identity: one live registration per worktree. Cleaned
-- sessions keep their row for history, so uniqueness is partial.
CREATE UNIQUE INDEX sessions_live_worktree
    ON sessions (worktree_path)
    WHERE status <> 'cleaned';

CREATE TABLE leases (
    id          INTEGER PRIMARY KEY,
    session_id  INTEGER NOT NULL REFERENCES sessions (id),
    path        TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('implicit', 'explicit')),
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER,
    released_at INTEGER,
    UNIQUE (session_id, path, kind)
);

CREATE INDEX leases_by_path ON leases (path) WHERE released_at IS NULL;

-- Snapshot of gate definitions (source of truth: .aethyme/gates.toml).
CREATE TABLE gates (
    name          TEXT PRIMARY KEY,
    command       TEXT NOT NULL,
    cost_tier     INTEGER NOT NULL DEFAULT 0,
    triggers_json TEXT NOT NULL DEFAULT '[]',
    updated_at    INTEGER NOT NULL
);

CREATE TABLE gate_results (
    id          INTEGER PRIMARY KEY,
    gate_name   TEXT NOT NULL,
    tree_hash   TEXT NOT NULL,
    status      TEXT NOT NULL CHECK (status IN ('pass', 'fail', 'cancelled', 'error')),
    exit_code   INTEGER,
    duration_ms INTEGER,
    log_path    TEXT,
    session_id  INTEGER REFERENCES sessions (id),
    created_at  INTEGER NOT NULL
);

-- Cache lookups: latest result for (gate, tree).
CREATE INDEX gate_results_by_gate_tree ON gate_results (gate_name, tree_hash, id);

CREATE TABLE merge_queue (
    id           INTEGER PRIMARY KEY,
    session_id   INTEGER NOT NULL REFERENCES sessions (id),
    head_commit  TEXT NOT NULL,
    base_commit  TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'submitted'
                 CHECK (status IN ('submitted', 'simulating', 'conflict',
                                   'verified', 'promoted', 'rejected', 'superseded')),
    merged_tree  TEXT,
    details_json TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    UNIQUE (session_id, head_commit)
);

-- Append-only. AUTOINCREMENT forbids rowid reuse so event ids are
-- strictly increasing forever (a replay/cursor guarantee, worth the
-- small insert cost).
CREATE TABLE events (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    schema_version INTEGER NOT NULL,
    ts             INTEGER NOT NULL,
    kind           TEXT NOT NULL,
    session_id     INTEGER,
    payload_json   TEXT
);

CREATE INDEX events_by_kind ON events (kind, id);
";

const MIGRATION_V2: &str = "
ALTER TABLE gate_results
ADD COLUMN failure_class TEXT
    CHECK (failure_class IS NULL OR failure_class IN (
        'test_failure',
        'environment',
        'resource_contention',
        'timeout',
        'cached_prior_fail',
        'unknown'
    ));
";

const MIGRATION_V3: &str = "
CREATE TABLE session_foreign_files (
    id          INTEGER PRIMARY KEY,
    session_id  INTEGER NOT NULL REFERENCES sessions (id),
    path        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    UNIQUE (session_id, path)
);

CREATE INDEX session_foreign_files_by_session
    ON session_foreign_files (session_id, path);
";

const MIGRATION_V4: &str = "
CREATE TABLE pr_watch_state (
    id                    INTEGER PRIMARY KEY,
    target_branch         TEXT NOT NULL,
    pr_number             INTEGER NOT NULL,
    activity_fingerprint  TEXT NOT NULL DEFAULT '',
    marker                TEXT NOT NULL DEFAULT 'none',
    last_dispatch_at      INTEGER,
    last_agent_session_id INTEGER,
    updated_at            INTEGER NOT NULL,
    UNIQUE (target_branch, pr_number)
);

CREATE INDEX pr_watch_state_by_target
    ON pr_watch_state (target_branch, pr_number);
";

const MIGRATION_V5: &str = "
-- SQLite cannot alter a CHECK constraint in place. Rebuild the queue so
-- externally landed promotions have a durable, queryable terminal state.
CREATE TABLE merge_queue_v5 (
    id           INTEGER PRIMARY KEY,
    session_id   INTEGER NOT NULL REFERENCES sessions (id),
    head_commit  TEXT NOT NULL,
    base_commit  TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'submitted'
                 CHECK (status IN ('submitted', 'simulating', 'conflict',
                                   'verified', 'promoted', 'externally_landed',
                                   'rejected', 'superseded')),
    merged_tree  TEXT,
    details_json TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    UNIQUE (session_id, head_commit)
);

INSERT INTO merge_queue_v5
SELECT id, session_id, head_commit, base_commit, status, merged_tree,
       details_json, created_at, updated_at
FROM merge_queue;
DROP TABLE merge_queue;
ALTER TABLE merge_queue_v5 RENAME TO merge_queue;

CREATE TABLE integration_reconciliations (
    id                   INTEGER PRIMARY KEY,
    upstream_ref         TEXT NOT NULL,
    local_main_commit    TEXT NOT NULL,
    old_integration      TEXT NOT NULL,
    upstream_commit      TEXT NOT NULL,
    new_integration      TEXT NOT NULL,
    created_at           INTEGER NOT NULL
);

CREATE TABLE integration_reconciliation_entries (
    id                    INTEGER PRIMARY KEY,
    reconciliation_id     INTEGER NOT NULL REFERENCES integration_reconciliations (id),
    queue_entry_id         INTEGER NOT NULL REFERENCES merge_queue (id),
    classification        TEXT NOT NULL CHECK (classification IN
                              ('already_landed', 'superseded_upstream',
                               'still_pending')),
    old_merge_commit      TEXT NOT NULL,
    upstream_landing      TEXT,
    replayed_commit       TEXT,
    details_json          TEXT,
    UNIQUE (reconciliation_id, queue_entry_id)
);

CREATE INDEX integration_reconciliation_entries_by_queue
    ON integration_reconciliation_entries (queue_entry_id, reconciliation_id);

-- Durable two-phase intent: if the process dies after moving the Git ref
-- but before committing queue rows, the next Broker::open can finish the
-- transaction. If the ref never moved, it safely discards the intent.
CREATE TABLE integration_reconciliation_intent (
    id                   INTEGER PRIMARY KEY CHECK (id = 1),
    branch               TEXT NOT NULL,
    upstream_ref         TEXT NOT NULL,
    local_main_commit    TEXT NOT NULL,
    old_integration      TEXT NOT NULL,
    upstream_commit      TEXT NOT NULL,
    new_integration      TEXT NOT NULL,
    created_at           INTEGER NOT NULL
);

CREATE TABLE integration_reconciliation_intent_entries (
    queue_entry_id         INTEGER PRIMARY KEY REFERENCES merge_queue (id),
    status                 TEXT NOT NULL,
    merged_tree            TEXT,
    details_json           TEXT NOT NULL,
    classification         TEXT NOT NULL,
    old_merge_commit       TEXT NOT NULL,
    upstream_landing       TEXT,
    replayed_commit        TEXT
);
";

const MIGRATION_V6: &str = "
CREATE TABLE coordinated_operations (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id   INTEGER NOT NULL REFERENCES sessions (id),
    provider     TEXT NOT NULL CHECK (provider IN ('git', 'github')),
    repository   TEXT NOT NULL,
    scope        TEXT NOT NULL,
    effect       TEXT NOT NULL CHECK (effect IN ('read', 'write', 'destructive')),
    authorization_reason TEXT,
    status       TEXT NOT NULL CHECK (status IN (
                     'prepared', 'running', 'succeeded', 'failed',
                     'outcome_unknown', 'reconciled_succeeded',
                     'reconciled_failed'
                 )),
    command_json TEXT NOT NULL,
    pid          INTEGER NOT NULL,
    exit_code    INTEGER,
    details_json TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    finished_at  INTEGER
);

CREATE INDEX coordinated_operations_by_repository
    ON coordinated_operations (repository, status, id);
CREATE INDEX coordinated_operations_by_session
    ON coordinated_operations (session_id, id);
";

const MIGRATION_V7: &str = "
ALTER TABLE integration_reconciliation_intent
    ADD COLUMN plan_digest TEXT NOT NULL DEFAULT '';
ALTER TABLE integration_reconciliations
    ADD COLUMN plan_digest TEXT NOT NULL DEFAULT '';
";

const MIGRATION_V8: &str = "
ALTER TABLE gates ADD COLUMN resources_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE gates ADD COLUMN resource_ttl_seconds INTEGER NOT NULL DEFAULT 300;
ALTER TABLE gates ADD COLUMN definition_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE gate_results ADD COLUMN definition_hash TEXT NOT NULL DEFAULT '';
CREATE INDEX gate_results_by_gate_tree_definition
    ON gate_results (gate_name, tree_hash, definition_hash, id);
";

const MIGRATION_V9: &str = "
-- `diff_base` may advance after an explicitly guarded reuse sync. Preserve
-- the original adoption boundary independently before that can happen.
ALTER TABLE sessions ADD COLUMN adoption_base TEXT;
UPDATE sessions SET adoption_base = diff_base;

-- Repository-contract fields are nullable for cleaned historical sessions
-- whose checkout may no longer exist. Broker::open backfills every live row
-- from the best available worktree snapshot before serving it.
ALTER TABLE sessions ADD COLUMN repository_schema INTEGER;
ALTER TABLE sessions ADD COLUMN deployment_state_digest TEXT;
ALTER TABLE sessions ADD COLUMN aethyme_version TEXT;
ALTER TABLE sessions ADD COLUMN gate_definition_digest TEXT;
ALTER TABLE sessions ADD COLUMN repository_contract_backfilled INTEGER NOT NULL DEFAULT 0
    CHECK (repository_contract_backfilled IN (0, 1));
";

const MIGRATION_V10: &str = "
-- Older operation rows used caller or clone-local repository spellings. Keep
-- them readable, but never treat their identity as suitable for host-wide
-- coordination without a fresh resolution.
ALTER TABLE coordinated_operations ADD COLUMN host_operation_id TEXT;
ALTER TABLE coordinated_operations ADD COLUMN identity_provenance TEXT NOT NULL
    DEFAULT 'legacy_unverified_identity'
    CHECK (identity_provenance IN (
        'legacy_unverified_identity', 'verified_canonical', 'local_repository'
    ));
CREATE UNIQUE INDEX coordinated_operations_by_host_operation
    ON coordinated_operations (host_operation_id)
    WHERE host_operation_id IS NOT NULL;
";

const MIGRATION_V11: &str = "
-- Adoption provenance and accepted contribution state are different facts.
-- Preserve the oldest durable provenance available, but do not infer that a
-- legacy diff baseline proves a contribution was promoted.
ALTER TABLE sessions ADD COLUMN adopted_head TEXT;
ALTER TABLE sessions ADD COLUMN accepted_session_head TEXT;
ALTER TABLE sessions ADD COLUMN accepted_integration_commit TEXT;
ALTER TABLE sessions ADD COLUMN accepted_integration_tree TEXT;
ALTER TABLE sessions ADD COLUMN accepted_queue_entry_id INTEGER
    REFERENCES merge_queue (id);
ALTER TABLE sessions ADD COLUMN accepted_at INTEGER;

UPDATE sessions
SET adopted_head = COALESCE(adoption_base, diff_base);

CREATE TRIGGER sessions_adopted_head_immutable
BEFORE UPDATE OF adopted_head ON sessions
WHEN OLD.adopted_head IS NOT NEW.adopted_head
BEGIN
    SELECT RAISE(ABORT, 'sessions.adopted_head is immutable');
END;
";

const MIGRATION_V12: &str = "
-- Gate resource contention is expected when independent clones share a host.
-- Persist the bounded wait policy so historical definitions remain auditable.
ALTER TABLE gates ADD COLUMN resource_wait_seconds INTEGER NOT NULL DEFAULT 0;
";

const MIGRATION_V13: &str = "
-- Broker-owned artifact cache policy is stored separately from generic host
-- resource declarations so old gate results remain explainable.
ALTER TABLE gates ADD COLUMN managed_cache_json TEXT;
";

const MIGRATION_V14: &str = "
-- Content-free execution telemetry separates coordination delay, command
-- startup, and logging volume without storing command output.
ALTER TABLE gate_results ADD COLUMN wait_duration_ms INTEGER;
ALTER TABLE gate_results ADD COLUMN first_output_ms INTEGER;
ALTER TABLE gate_results ADD COLUMN output_bytes INTEGER;
";

const MIGRATION_V15: &str = "
-- Operation history is paged newest-first. Each optional selector gets an
-- id-suffixed index so SQLite can filter and walk the page in cursor order.
CREATE INDEX coordinated_operations_history_by_id
    ON coordinated_operations (id DESC);
CREATE INDEX coordinated_operations_history_by_session
    ON coordinated_operations (session_id, id DESC);
CREATE INDEX coordinated_operations_history_by_status
    ON coordinated_operations (status, id DESC);
CREATE INDEX coordinated_operations_history_by_repository
    ON coordinated_operations (repository, id DESC);
CREATE INDEX coordinated_operations_history_by_provider
    ON coordinated_operations (provider, id DESC);
";

const MIGRATION_V16: &str = "
-- Non-blocking advisories are durable facts. Markdown is only a projection
-- of rows whose resolution state remains outstanding.
CREATE TABLE advisories (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    identity         TEXT NOT NULL UNIQUE,
    session_id       INTEGER REFERENCES sessions (id),
    severity         TEXT NOT NULL CHECK (severity IN ('info', 'warning', 'critical')),
    queue_entry_id   INTEGER REFERENCES merge_queue (id),
    integration_sha  TEXT,
    paths_json       TEXT NOT NULL,
    evidence_json    TEXT NOT NULL,
    created_at       INTEGER NOT NULL,
    resolution_state TEXT NOT NULL DEFAULT 'outstanding'
                     CHECK (resolution_state IN ('outstanding', 'acknowledged')),
    acknowledged_at  INTEGER
);

CREATE INDEX advisories_by_resolution
    ON advisories (resolution_state, id DESC);
CREATE INDEX advisories_by_session
    ON advisories (session_id, id DESC);
CREATE INDEX advisories_by_queue_entry
    ON advisories (queue_entry_id, id DESC);
";

const MIGRATIONS: &[&str] = &[
    MIGRATION_V1,
    MIGRATION_V2,
    MIGRATION_V3,
    MIGRATION_V4,
    MIGRATION_V5,
    MIGRATION_V6,
    MIGRATION_V7,
    MIGRATION_V8,
    MIGRATION_V9,
    MIGRATION_V10,
    MIGRATION_V11,
    MIGRATION_V12,
    MIGRATION_V13,
    MIGRATION_V14,
    MIGRATION_V15,
    MIGRATION_V16,
];

pub(crate) fn current_version(conn: &Connection) -> Result<i64, BrokerError> {
    let version = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(|v| v.parse::<i64>().unwrap_or(0))
        .unwrap_or(0);
    Ok(version)
}

/// Apply pending migrations. Called on every open; cheap when current.
///
/// Concurrency: several CLI processes may open a fresh database at the
/// same moment. The version check is therefore repeated *inside* each
/// `BEGIN IMMEDIATE` transaction — the write lock serializes racers, and
/// the loser re-reads the version and skips work the winner already did.
pub fn migrate(conn: &Connection) -> Result<(), BrokerError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    )?;

    let found = current_version(conn)?;
    if found > SCHEMA_VERSION {
        return Err(BrokerError::SchemaTooNew {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    if found == SCHEMA_VERSION {
        return Ok(());
    }

    for (index, sql) in MIGRATIONS.iter().enumerate() {
        let version = (index + 1) as i64;
        // One transaction per migration so a crash leaves a consistent,
        // resumable state.
        conn.execute_batch("BEGIN IMMEDIATE")?;
        if current_version(conn)? >= version {
            // Another process already applied this migration.
            conn.execute_batch("COMMIT")?;
            continue;
        }
        let applied = conn.execute_batch(sql).and_then(|()| {
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [version.to_string()],
            )
        });
        match applied {
            Ok(_) => conn.execute_batch("COMMIT")?,
            Err(err) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(err.into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v15_adds_newest_first_operation_history_indexes() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let mut statement = conn
            .prepare("PRAGMA index_list('coordinated_operations')")
            .unwrap();
        let indexes = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for expected in [
            "coordinated_operations_history_by_id",
            "coordinated_operations_history_by_session",
            "coordinated_operations_history_by_status",
            "coordinated_operations_history_by_repository",
            "coordinated_operations_history_by_provider",
        ] {
            assert!(indexes.iter().any(|index| index == expected), "{expected}");
        }
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v16_adds_typed_advisory_storage_to_a_v15_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..15].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }

        migrate(&conn).unwrap();

        let columns = conn
            .prepare("PRAGMA table_info(advisories)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for expected in [
            "identity",
            "session_id",
            "severity",
            "queue_entry_id",
            "integration_sha",
            "paths_json",
            "evidence_json",
            "created_at",
            "resolution_state",
            "acknowledged_at",
        ] {
            assert!(
                columns.iter().any(|column| column == expected),
                "{expected}"
            );
        }
        assert_eq!(current_version(&conn).unwrap(), 16);
    }

    #[test]
    fn v9_preserves_the_original_baseline_for_live_pre_contract_sessions() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..8].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO sessions (
                worktree_path, branch, origin, status, task, diff_base,
                created_at, updated_at, last_activity_at
             ) VALUES ('/repo/worktree', 'agent/live', 'adopted', 'active',
                       'task', 'original-sha', 1, 1, 1)",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let row = conn
            .query_row(
                "SELECT adoption_base, deployment_state_digest,
                        repository_contract_backfilled
                 FROM sessions WHERE branch = 'agent/live'",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0.as_deref(), Some("original-sha"));
        assert_eq!(
            row.1, None,
            "filesystem-dependent backfill runs in Broker::open"
        );
        assert!(!row.2);
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v10_marks_existing_operation_identity_as_legacy_unverified() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..9].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO sessions (
                worktree_path, branch, origin, status, created_at, updated_at,
                last_activity_at
             ) VALUES ('/repo', 'agent/legacy', 'adopted', 'cleaned', 1, 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO coordinated_operations (
                session_id, provider, repository, scope, effect, status,
                command_json, pid, created_at, updated_at
             ) VALUES (1, 'git', 'GitHub.com/Owner/Repo', 'repository', 'write',
                       'succeeded', '[]', 1, 1, 1)",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let row: (Option<String>, String) = conn
            .query_row(
                "SELECT host_operation_id, identity_provenance
                 FROM coordinated_operations WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row.0, None);
        assert_eq!(row.1, "legacy_unverified_identity");
    }

    #[test]
    fn v11_preserves_adoption_provenance_without_inventing_acceptance() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..10].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO sessions (
                worktree_path, branch, origin, status, diff_base, adoption_base,
                created_at, updated_at, last_activity_at
             ) VALUES ('/repo', 'agent/legacy', 'adopted', 'active',
                       'refreshed-diff-base', 'original-adopted-head', 1, 2, 2)",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let row = conn
            .query_row(
                "SELECT adopted_head, accepted_session_head,
                        accepted_integration_commit, accepted_integration_tree,
                        accepted_queue_entry_id, accepted_at
                 FROM sessions WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0.as_deref(), Some("original-adopted-head"));
        assert_eq!(
            (row.1, row.2, row.3, row.4, row.5),
            (None, None, None, None, None)
        );
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);

        let err = conn
            .execute(
                "UPDATE sessions SET adopted_head = 'rewritten' WHERE id = 1",
                [],
            )
            .unwrap_err();
        assert!(err.to_string().contains("adopted_head is immutable"));
    }
}
