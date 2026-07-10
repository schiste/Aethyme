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
pub const SCHEMA_VERSION: i64 = 1;

/// Version stamped on every event row written by this binary.
pub const EVENTS_SCHEMA_VERSION: i64 = 1;

const MIGRATION_V1: &str = "
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

const MIGRATIONS: &[&str] = &[MIGRATION_V1];

fn current_version(conn: &Connection) -> Result<i64, BrokerError> {
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
