//! Schema v1 for `.aethyme/broker.db`, plus migration machinery.
//!
//! Migration rules:
//! - `meta.schema_version` records the applied version; migrations run in
//!   order inside one transaction per version.
//! - Migrations are append-only: never edit an entry in [`MIGRATIONS`],
//!   only add new ones.
//! - A version number names a migration only once it is on the default
//!   branch. A build from an unmerged branch may have stamped the same number
//!   onto a real database with different contents (2026-10-07: #564's "v48"
//!   added `agent_provenance_json`, so #611's v48 tables were skipped and
//!   #613's v49 then failed with "duplicate column name"). Migrations from
//!   v48 on are therefore applied idempotently, and every open runs a repair
//!   pass that re-creates their objects when a database claims the version
//!   but lacks them. Never point a dev build at a real broker database.
//! - `meta.min_compatible_schema` records the oldest schema version whose
//!   binaries may still read and write the database. A binary older than the
//!   database opens it without migrating when its own [`SCHEMA_VERSION`] is at
//!   least that value, and otherwise fails with [`BrokerError::SchemaTooNew`].
//!   Raise [`MIN_COMPATIBLE_SCHEMA`] in the same change as a migration older
//!   writers cannot tolerate, or when older writer behavior would violate a
//!   new durable-data invariant. Structural examples include a renamed or
//!   dropped column, a new `NOT NULL` column without a default, or a changed
//!   constraint. New tables, indexes and nullable/defaulted columns alone leave
//!   it unchanged.
//! - The `events` table is append-only by contract: the store exposes no
//!   update or delete for it, and each row carries its own
//!   `schema_version` ([`EVENTS_SCHEMA_VERSION`]) so old rows stay
//!   interpretable after the event contract evolves.

use rusqlite::Connection;

use crate::error::BrokerError;

/// Current database schema version (== `MIGRATIONS.len()`).
pub const SCHEMA_VERSION: i64 = 50;

/// The oldest schema version whose binaries can safely use a database at
/// [`SCHEMA_VERSION`]. Before this existed every newer database locked out
/// every older binary, including for purely additive migrations (#293).
///
/// Migrations declared compatible (left at the previous minimum):
/// - v43: nullable machine-environment columns on `gate_results`. A v42
///   writer names its columns explicitly, so its rows simply leave them NULL.
/// - v44: nullable session short names; older binaries continue to ignore the
///   additional column.
/// - v45: three new tables for lifecycle telemetry. A writer that predates
///   them never names them, so they stay empty and every figure derived from
///   them is reported as unmeasured rather than as zero. Nothing existing is
///   renamed, retyped or given a new constraint, which is what would force
///   older binaries out.
/// - v46: one new table for named ownership claims. An older binary never
///   names it, so it neither sees nor writes claims.
/// - v47: keeps the existing `gate_results` columns, adds nullable clear
///   metadata, makes result ids AUTOINCREMENT, and seeds the sequence above
///   historical gate events. The minimum rises to v47 because an older
///   binary never reads `cleared_at`: it would keep serving a cleared failure
///   as a cached verdict and listing it as a blocker, and its `unblock` DELETE
///   is refused by the trigger below, so it could never clear it. The
///   migration fences already-open v46 writers with that DELETE trigger, and
///   records the new floor in the same transaction as the v47 schema marker.
/// - v48: five new tables for repository-wide pull request watches and their
///   deliveries (#606). An older binary never names them, so it neither polls
///   repository watches nor claims their deliveries; nothing existing changes.
/// - v49: adds nullable local operation provenance. A v48 writer names its
///   existing columns and can continue writing; a v48 reader safely ignores
///   the additional field.
/// - v50: no schema change. It marks the first binaries that implement
///   `[collaboration] capture = "required"` (#660). Repositories that do not
///   require capture keep this minimum; one that does has its own floor
///   raised to [`COLLABORATION_FENCE_SCHEMA`] (see that constant).
pub const MIN_COMPATIBLE_SCHEMA: i64 = 47;

/// The first schema whose binaries implement required collaboration capture
/// (#660): the version of [`MIGRATION_V50`].
///
/// A repository whose effective config says `capture = "required"` has its
/// `meta.min_compatible_schema` raised to this value, so every older binary
/// refuses its database with [`BrokerError::SchemaTooNew`] instead of
/// submitting uncaptured work. The floor is never lowered, so turning required
/// off later does not let older binaries back in. If this migration is
/// renumbered on rebase, this constant must follow it; a test pins the pair.
pub const COLLABORATION_FENCE_SCHEMA: i64 = 50;

/// Why a database carries the collaboration fence (`meta.collaboration_fence`).
pub const COLLABORATION_FENCE_REASON: &str = "collaboration capture required";

/// The collaboration fence on a database: raised, or required by the
/// committed config but not yet raised.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CollaborationFence {
    /// The floor in force (`active`), or the one to be raised (`pending`).
    pub min_compatible_schema: i64,
    pub reason: String,
    /// The config that required it: `committed` (the fetched default
    /// branch's `.aethyme/config.toml`).
    pub source: String,
    /// `active` once raised; `pending` while required but not yet raised,
    /// because this open is read-only or the write failed.
    pub state: &'static str,
}

impl CollaborationFence {
    /// Required by the committed config, not yet raised.
    pub fn pending() -> Self {
        Self {
            min_compatible_schema: COLLABORATION_FENCE_SCHEMA,
            reason: COLLABORATION_FENCE_REASON.into(),
            source: "committed".into(),
            state: "pending",
        }
    }
}

/// Read the fence in one query: the floor, the reason and the source.
pub fn collaboration_fence(conn: &Connection) -> Result<Option<CollaborationFence>, BrokerError> {
    let (minimum, reason, source): (Option<String>, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT (SELECT value FROM meta WHERE key = 'min_compatible_schema'),
                    (SELECT value FROM meta WHERE key = 'collaboration_fence'),
                    (SELECT value FROM meta WHERE key = 'collaboration_fence_source')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let minimum = minimum.and_then(|value| value.parse::<i64>().ok());
    Ok(match (minimum, reason) {
        (Some(minimum), Some(reason)) if minimum >= COLLABORATION_FENCE_SCHEMA => {
            Some(CollaborationFence {
                min_compatible_schema: minimum,
                reason,
                source: source.unwrap_or_else(|| "unknown".into()),
                state: "active",
            })
        }
        _ => None,
    })
}

/// Raise the floor to [`COLLABORATION_FENCE_SCHEMA`] and record why and from
/// which config, in one transaction that rolls back on any failure, so a
/// failed raise never leaves the connection holding a write lock. Never
/// lowers a higher floor and never replaces a recorded reason or source.
pub fn raise_collaboration_fence(conn: &Connection, source: &str) -> Result<(), BrokerError> {
    let transaction = conn.unchecked_transaction()?;
    transaction.execute(
        "INSERT INTO meta (key, value) VALUES ('min_compatible_schema', ?1)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value
         WHERE CAST(meta.value AS INTEGER) < CAST(excluded.value AS INTEGER)",
        [COLLABORATION_FENCE_SCHEMA.to_string()],
    )?;
    for (key, value) in [
        ("collaboration_fence", COLLABORATION_FENCE_REASON),
        ("collaboration_fence_source", source),
    ] {
        transaction.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO NOTHING",
            [key, value],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

/// Whether this binary may use a database at `found`, a version newer than
/// its own, because every migration past [`SCHEMA_VERSION`] was declared
/// compatible. A database without the marker predates it and is refused.
pub fn newer_schema_is_compatible(conn: &Connection, found: i64) -> Result<bool, BrokerError> {
    schema_is_compatible_with(conn, found, SCHEMA_VERSION)
}

/// [`newer_schema_is_compatible`] for a binary whose own schema version is
/// `supported`, so the decision an older binary makes can be tested here.
pub(crate) fn schema_is_compatible_with(
    conn: &Connection,
    found: i64,
    supported: i64,
) -> Result<bool, BrokerError> {
    if found <= supported {
        return Ok(true);
    }
    let minimum: Option<i64> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'min_compatible_schema'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|value| value.parse().ok());
    Ok(minimum.is_some_and(|minimum| supported >= minimum))
}

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

const MIGRATION_V17: &str = "
-- Promoted paths remain an entry-level exposure until publication is proven.
-- Advisory acknowledgement is an operator action; verified publication uses
-- a separate terminal state with its own durable evidence.
DROP INDEX advisories_by_resolution;
DROP INDEX advisories_by_session;
DROP INDEX advisories_by_queue_entry;
ALTER TABLE advisories RENAME TO advisories_v16;

CREATE TABLE advisories (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    identity            TEXT NOT NULL UNIQUE,
    session_id          INTEGER REFERENCES sessions (id),
    severity            TEXT NOT NULL CHECK (severity IN ('info', 'warning', 'critical')),
    queue_entry_id      INTEGER REFERENCES merge_queue (id),
    integration_sha     TEXT,
    paths_json          TEXT NOT NULL,
    evidence_json       TEXT NOT NULL,
    created_at          INTEGER NOT NULL,
    resolution_state    TEXT NOT NULL DEFAULT 'outstanding'
                        CHECK (resolution_state IN ('outstanding', 'acknowledged', 'resolved')),
    acknowledged_at     INTEGER,
    resolved_at         INTEGER,
    resolution_evidence TEXT
);

INSERT INTO advisories (
    id, identity, session_id, severity, queue_entry_id, integration_sha,
    paths_json, evidence_json, created_at, resolution_state, acknowledged_at
)
SELECT id, identity, session_id, severity, queue_entry_id, integration_sha,
       paths_json, evidence_json, created_at, resolution_state, acknowledged_at
FROM advisories_v16;
DROP TABLE advisories_v16;

CREATE INDEX advisories_by_resolution
    ON advisories (resolution_state, id DESC);
CREATE INDEX advisories_by_session
    ON advisories (session_id, id DESC);
CREATE INDEX advisories_by_queue_entry
    ON advisories (queue_entry_id, id DESC);

CREATE TABLE entry_path_exposures (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    queue_entry_id      INTEGER NOT NULL UNIQUE REFERENCES merge_queue (id),
    promotion_sha       TEXT NOT NULL,
    paths_json          TEXT NOT NULL,
    created_at          INTEGER NOT NULL,
    state               TEXT NOT NULL DEFAULT 'outstanding'
                        CHECK (state IN ('outstanding', 'resolved')),
    resolved_at         INTEGER,
    resolution_kind     TEXT CHECK (resolution_kind IS NULL OR resolution_kind IN (
                            'ship_verified', 'external_reconciliation'
                        )),
    resolution_sha      TEXT,
    resolution_evidence TEXT
);

CREATE INDEX entry_path_exposures_by_state
    ON entry_path_exposures (state, id);
";

const MIGRATION_V18: &str = "
-- Session notes are repository-local coordination messages. Events retain
-- only redacted routing metadata; message text lives solely in this table.
CREATE TABLE session_notes (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    sender_session_id    INTEGER NOT NULL REFERENCES sessions (id),
    recipient_session_id INTEGER NOT NULL REFERENCES sessions (id),
    message              TEXT NOT NULL,
    created_at           INTEGER NOT NULL,
    acknowledged_at      INTEGER
);

CREATE INDEX session_notes_by_recipient
    ON session_notes (recipient_session_id, acknowledged_at, id DESC);
CREATE INDEX session_notes_by_sender
    ON session_notes (sender_session_id, id DESC);
";

const MIGRATION_V19: &str = "
-- Logical closure and physical artifact reclamation are separate lifecycle
-- transitions. Existing terminal sessions are conservatively backfilled as
-- closed: cleanup can prove and complete absent/retained artifacts later.
ALTER TABLE sessions ADD COLUMN cleanup_state TEXT NOT NULL DEFAULT 'open'
    CHECK (cleanup_state IN ('open', 'closed', 'cleaned'));
ALTER TABLE sessions ADD COLUMN closed_at INTEGER;
ALTER TABLE sessions ADD COLUMN cleanup_completed_at INTEGER;

UPDATE sessions
SET cleanup_state = 'closed', closed_at = updated_at
WHERE status = 'cleaned';
";

const MIGRATION_V20: &str = "
-- Retention planning walks age cutoffs and terminal status without scanning
-- the append-only tables from the beginning on every bounded maintenance run.
CREATE INDEX events_by_retention_age ON events (ts, id);
CREATE INDEX gate_results_by_retention_age ON gate_results (created_at, id);
CREATE INDEX merge_queue_by_retention_status_age
    ON merge_queue (status, updated_at, id);
CREATE INDEX sessions_by_cleanup_age
    ON sessions (cleanup_state, closed_at, id);
";

const MIGRATION_V21: &str = "
-- Default status reads only the latest row for each live session; terminal
-- history is served separately through a newest-first id cursor.
CREATE INDEX merge_queue_by_session_id
    ON merge_queue (session_id, id DESC);
";

const MIGRATION_V22: &str = "
-- Bounded, content-free shown-to-action correlation. One row per advisory
-- and delivery surface is updated in place rather than appending on every
-- command. Foreign-key cleanup follows the authoritative advisory row.
CREATE TABLE advisory_delivery_metrics (
    advisory_id     INTEGER NOT NULL REFERENCES advisories(id) ON DELETE CASCADE,
    session_id      INTEGER REFERENCES sessions(id),
    surface         TEXT NOT NULL CHECK (surface IN (
                        'status', 'command', 'post_commit', 'pre_gate', 'inventory'
                    )),
    first_shown_at  INTEGER NOT NULL,
    last_shown_at   INTEGER NOT NULL,
    show_count      INTEGER NOT NULL DEFAULT 1,
    acted_at        INTEGER,
    action          TEXT CHECK (action IN ('acknowledged', 'publication_resolved')),
    PRIMARY KEY (advisory_id, surface)
);
CREATE INDEX advisory_delivery_by_action
    ON advisory_delivery_metrics (acted_at, advisory_id);
";

const MIGRATION_V23: &str = "
-- Authenticated provider adapters submit only a strict normalized envelope.
-- Raw webhook bodies, comments, credentials, diffs, and task text have no
-- columns and therefore cannot enter broker storage accidentally.
CREATE TABLE external_coordination_events (
    id                           INTEGER PRIMARY KEY AUTOINCREMENT,
    provider                     TEXT NOT NULL CHECK (provider IN ('github')),
    provider_event_id            TEXT NOT NULL,
    event_type                   TEXT NOT NULL,
    repository                   TEXT NOT NULL,
    target_branch                TEXT NOT NULL,
    pr_number                    INTEGER NOT NULL,
    commit_sha                   TEXT NOT NULL,
    occurred_at                  INTEGER NOT NULL,
    verification_method          TEXT NOT NULL CHECK (verification_method IN (
                                     'webhook_signature', 'authenticated_poll'
                                 )),
    verified_at                  INTEGER NOT NULL,
    normalized_digest            TEXT NOT NULL,
    status                       TEXT NOT NULL CHECK (status IN (
                                     'pending_advisory', 'advisory_created',
                                     'unknown_event_type', 'unknown_pull_request',
                                     'owner_not_found', 'ambiguous_owner',
                                     'repository_mismatch', 'stale', 'ignored'
                                 )),
    session_id                   INTEGER REFERENCES sessions(id),
    queue_entry_id               INTEGER REFERENCES merge_queue(id),
    advisory_id                  INTEGER REFERENCES advisories(id),
    received_at                  INTEGER NOT NULL,
    reconciled_at                INTEGER,
    reconciliation_kind          TEXT CHECK (reconciliation_kind IN ('assigned', 'ignored')),
    reconciliation_reason_digest TEXT,
    UNIQUE (provider, provider_event_id)
);

CREATE INDEX external_events_by_status
    ON external_coordination_events (status, id DESC);
CREATE INDEX external_events_by_session
    ON external_coordination_events (session_id, id DESC);
CREATE INDEX external_events_by_pr
    ON external_coordination_events (target_branch, pr_number, id DESC);
CREATE INDEX external_events_by_commit
    ON external_coordination_events (commit_sha, id DESC);
";

const MIGRATION_V24: &str = "
-- Opt-in review coordination is repository policy, but its accepted
-- session/queue/commit/PR provenance and every state transition are durable.
CREATE TABLE review_lifecycles (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id          INTEGER NOT NULL UNIQUE REFERENCES sessions(id),
    queue_entry_id      INTEGER REFERENCES merge_queue(id),
    repository          TEXT NOT NULL,
    target_branch       TEXT NOT NULL,
    pr_number           INTEGER NOT NULL,
    commit_sha          TEXT NOT NULL,
    state               TEXT NOT NULL CHECK (state IN (
                            'draft_opened', 'local_submission_verified',
                            'review_requested', 'changes_requested',
                            'replacement_commit_submitted', 'review_satisfied',
                            'validation_unlocked'
                        )),
    generation          INTEGER NOT NULL DEFAULT 0,
    evidence_digest     TEXT,
    unlock_operation_id INTEGER REFERENCES coordinated_operations(id),
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    UNIQUE (repository, pr_number)
);

CREATE INDEX review_lifecycles_by_queue
    ON review_lifecycles (queue_entry_id);
CREATE INDEX review_lifecycles_by_commit
    ON review_lifecycles (commit_sha);

CREATE TABLE review_lifecycle_transitions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    lifecycle_id    INTEGER NOT NULL REFERENCES review_lifecycles(id),
    from_state      TEXT,
    to_state        TEXT NOT NULL,
    commit_sha      TEXT NOT NULL,
    queue_entry_id  INTEGER REFERENCES merge_queue(id),
    evidence_digest TEXT,
    operation_id    INTEGER REFERENCES coordinated_operations(id),
    created_at      INTEGER NOT NULL
);

CREATE INDEX review_transitions_by_lifecycle
    ON review_lifecycle_transitions (lifecycle_id, id);
";

const MIGRATION_V25: &str = "
-- Review lifecycles remain auditable after explicit abandonment while the
-- active session and PR identities become reusable. Rebuild both parent and
-- child tables so foreign-key integrity is preserved throughout migration.
CREATE TABLE review_lifecycles_v25 (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id          INTEGER NOT NULL REFERENCES sessions(id),
    queue_entry_id      INTEGER REFERENCES merge_queue(id),
    repository          TEXT NOT NULL,
    target_branch       TEXT NOT NULL,
    pr_number           INTEGER NOT NULL,
    commit_sha          TEXT NOT NULL,
    state               TEXT NOT NULL CHECK (state IN (
                            'draft_opened', 'local_submission_verified',
                            'review_requested', 'changes_requested',
                            'replacement_commit_submitted', 'review_satisfied',
                            'validation_unlocked'
                        )),
    generation          INTEGER NOT NULL DEFAULT 0,
    evidence_digest     TEXT,
    unlock_operation_id INTEGER REFERENCES coordinated_operations(id),
    active              INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    abandoned_at        INTEGER,
    abandon_reason_digest TEXT,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL
);

INSERT INTO review_lifecycles_v25 (
    id, session_id, queue_entry_id, repository, target_branch, pr_number,
    commit_sha, state, generation, evidence_digest, unlock_operation_id,
    active, abandoned_at, abandon_reason_digest, created_at, updated_at
)
SELECT id, session_id, queue_entry_id, repository, target_branch, pr_number,
       commit_sha, state, generation, evidence_digest, unlock_operation_id,
       1, NULL, NULL, created_at, updated_at
FROM review_lifecycles;

CREATE TABLE review_lifecycle_transitions_v25 (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    lifecycle_id    INTEGER NOT NULL REFERENCES review_lifecycles_v25(id),
    from_state      TEXT,
    to_state        TEXT NOT NULL,
    commit_sha      TEXT NOT NULL,
    queue_entry_id  INTEGER REFERENCES merge_queue(id),
    evidence_digest TEXT,
    operation_id    INTEGER REFERENCES coordinated_operations(id),
    created_at      INTEGER NOT NULL
);

INSERT INTO review_lifecycle_transitions_v25
SELECT * FROM review_lifecycle_transitions;

DROP TABLE review_lifecycle_transitions;
DROP TABLE review_lifecycles;
ALTER TABLE review_lifecycles_v25 RENAME TO review_lifecycles;
ALTER TABLE review_lifecycle_transitions_v25 RENAME TO review_lifecycle_transitions;

CREATE UNIQUE INDEX review_lifecycles_active_session
    ON review_lifecycles (session_id) WHERE active = 1;
CREATE UNIQUE INDEX review_lifecycles_active_pr
    ON review_lifecycles (repository, pr_number) WHERE active = 1;
CREATE INDEX review_lifecycles_by_queue
    ON review_lifecycles (queue_entry_id);
CREATE INDEX review_lifecycles_by_commit
    ON review_lifecycles (commit_sha);
CREATE INDEX review_transitions_by_lifecycle
    ON review_lifecycle_transitions (lifecycle_id, id);
";

const MIGRATION_V26: &str = "
CREATE TABLE pull_request_watches (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id           INTEGER NOT NULL REFERENCES sessions(id),
    provider             TEXT NOT NULL CHECK (provider IN ('github')),
    canonical_repository TEXT NOT NULL,
    display_repository   TEXT NOT NULL,
    pr_number            INTEGER NOT NULL CHECK (pr_number > 0),
    target_branch        TEXT NOT NULL,
    head_sha             TEXT NOT NULL,
    is_draft             INTEGER NOT NULL CHECK (is_draft IN (0, 1)),
    status               TEXT NOT NULL CHECK (status IN ('active', 'paused', 'completed', 'stopped')),
    event_kinds_json     TEXT NOT NULL,
    poll_interval_seconds INTEGER NOT NULL CHECK (poll_interval_seconds BETWEEN 15 AND 3600),
    cursor_digest        TEXT NOT NULL,
    last_polled_at       INTEGER,
    next_poll_at         INTEGER,
    last_error_code      TEXT,
    created_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL
);

CREATE UNIQUE INDEX pull_request_watches_live_pr
    ON pull_request_watches (canonical_repository, pr_number)
    WHERE status IN ('active', 'paused');
CREATE INDEX pull_request_watches_by_session
    ON pull_request_watches (session_id, id);
CREATE INDEX pull_request_watches_due
    ON pull_request_watches (status, next_poll_at, id);
";

const MIGRATION_V27: &str = "
CREATE TABLE pull_request_activities (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    watch_id            INTEGER NOT NULL REFERENCES pull_request_watches(id),
    kind                TEXT NOT NULL CHECK (kind IN ('comment', 'review', 'check')),
    provider_id         TEXT NOT NULL,
    author              TEXT,
    state               TEXT,
    url                 TEXT,
    provider_updated_at TEXT,
    first_seen_at       INTEGER NOT NULL,
    last_seen_at        INTEGER NOT NULL,
    UNIQUE (watch_id, kind, provider_id)
);

CREATE INDEX pull_request_activities_by_watch
    ON pull_request_activities (watch_id, id);

CREATE TABLE pull_request_activity_batches (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    watch_id          INTEGER NOT NULL REFERENCES pull_request_watches(id),
    head_sha          TEXT NOT NULL,
    digest            TEXT NOT NULL,
    activity_count    INTEGER NOT NULL CHECK (activity_count > 0),
    status            TEXT NOT NULL CHECK (status IN ('pending', 'acknowledged')),
    ack_outcome       TEXT CHECK (ack_outcome IS NULL OR ack_outcome IN (
                          'addressed', 'stale', 'non_actionable', 'superseded')),
    ack_reason_digest TEXT,
    created_at        INTEGER NOT NULL,
    acknowledged_at  INTEGER,
    UNIQUE (watch_id, digest)
);

CREATE TABLE pull_request_activity_batch_items (
    batch_id   INTEGER NOT NULL REFERENCES pull_request_activity_batches(id),
    activity_id INTEGER NOT NULL REFERENCES pull_request_activities(id),
    PRIMARY KEY (batch_id, activity_id)
);

CREATE INDEX pull_request_activity_batches_pending
    ON pull_request_activity_batches (status, id);
";

const MIGRATION_V28: &str = "
CREATE TABLE delivery_subscriptions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    watch_id   INTEGER NOT NULL REFERENCES pull_request_watches(id),
    adapter    TEXT NOT NULL,
    target     TEXT NOT NULL,
    policy     TEXT NOT NULL CHECK (policy IN ('notify', 'resume', 'review_and_push')),
    active     INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (watch_id, adapter, target)
);

CREATE TABLE delivery_outbox (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    subscription_id    INTEGER NOT NULL REFERENCES delivery_subscriptions(id),
    batch_id           INTEGER NOT NULL REFERENCES pull_request_activity_batches(id),
    status             TEXT NOT NULL CHECK (status IN ('pending', 'claimed', 'delivered', 'failed')),
    generation         INTEGER NOT NULL DEFAULT 0,
    claimed_by         TEXT,
    claim_expires_at   INTEGER,
    attempt_count      INTEGER NOT NULL DEFAULT 0,
    last_error_code    TEXT,
    delivered_at       INTEGER,
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL,
    UNIQUE (subscription_id, batch_id)
);

CREATE INDEX delivery_outbox_due
    ON delivery_outbox (status, claim_expires_at, id);
CREATE INDEX delivery_outbox_by_adapter
    ON delivery_outbox (subscription_id, status, id);
";

const MIGRATION_V29: &str = "
-- Advisory delivery and provenance are typed independently from free-form
-- identities. Existing rows are session coordination notices by contract.
ALTER TABLE advisories ADD COLUMN audience TEXT NOT NULL DEFAULT 'session'
    CHECK (audience IN ('session', 'maintainer'));
ALTER TABLE advisories ADD COLUMN producer TEXT NOT NULL DEFAULT 'coordination'
    CHECK (producer IN (
        'coordination', 'conflict_history', 'gate_reliability_history',
        'isolation_history', 'resource_history'
    ));
CREATE INDEX advisories_by_audience_resolution
    ON advisories (audience, resolution_state, id DESC);
CREATE INDEX advisories_by_producer_identity
    ON advisories (producer, identity);
";

const MIGRATION_V30: &str = "
-- Suppression is a maintainer-only control layered over the established
-- acknowledged state, so widening the durable state constraint is unnecessary.
ALTER TABLE advisories ADD COLUMN suppressed_at INTEGER;
";

// Commit attribution: the promote merge commit must credit the agent that did
// the work, not just the broker that applied it. Identity is per-session
// because concurrent sessions may run different agents -- a repo-level setting
// could not tell them apart. NULL means "unknown", and an unknown agent is
// omitted from the trailers rather than guessed.
const MIGRATION_V31: &str = "
ALTER TABLE sessions ADD COLUMN agent_identity TEXT;
";

// Representation of work that reached the default branch through a provider-side
// merge rather than through `broker submit`. Ancestry cannot record this: a
// squash or rebase merge produces a commit with a new SHA and no relationship to
// the session's commits. The representing commit is stored rather than
// recomputed because a verdict against a fixed historical commit stays true,
// while the same verdict against the branch tip decays the moment an unrelated
// change touches one of the same files.
//
// `representing_commit` is NULL only for a session whose net content the branch
// already held, where naming a commit would misattribute work that commit did
// not carry.
const MIGRATION_V32: &str = "
CREATE TABLE session_representations (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id          INTEGER NOT NULL REFERENCES sessions(id),
    session_head        TEXT NOT NULL,
    representing_commit TEXT,
    representing_ref    TEXT NOT NULL,
    discovery           TEXT NOT NULL CHECK (discovery IN ('merge_time', 'history_walk')),
    pr_number           INTEGER CHECK (pr_number IS NULL OR pr_number > 0),
    paths_json          TEXT NOT NULL,
    evidence            TEXT NOT NULL,
    created_at          INTEGER NOT NULL
);

CREATE UNIQUE INDEX session_representations_head
    ON session_representations (session_id, session_head);
";

/// The review spend ledger: what the router has already asked for, per pull
/// request and per review dimension.
///
/// Scheduling already refuses to re-request a review bound to the head it was
/// requested for. The unique index makes that refusal durable rather than only
/// decided: an executor that died between writing the row and spawning the
/// reviewer re-runs and hits the index, so a crash costs a lost reviewer rather
/// than a duplicated one. `backend` records who was asked, because the same
/// dimension can be routed differently as policy changes and "why is there no
/// review" is otherwise unanswerable after the fact.
const MIGRATION_V33: &str = "
CREATE TABLE review_requests (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    repository      TEXT NOT NULL,
    pr_number       INTEGER NOT NULL,
    review_type     TEXT NOT NULL,
    head_commit     TEXT NOT NULL,
    backend         TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN (
                        'requested', 'running', 'satisfied', 'failed', 'abandoned')),
    detail          TEXT,
    requested_at    INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);

CREATE UNIQUE INDEX review_requests_head
    ON review_requests (repository, pr_number, review_type, head_commit);

CREATE INDEX review_requests_by_pr
    ON review_requests (repository, pr_number);
";

/// Split "the record is the whole outcome" out of "nobody was ever asked".
///
/// v33 spelled both `abandoned`, which forced the executor to read `backend` to
/// tell a settled review from a retryable one. That is the ambiguity a ledger
/// exists to remove: someone reading this table in a year has only the row.
///
/// `recorded` is now what the `record` backend produces -- the policy asked for
/// nothing to be performed, and the row is the complete answer. `abandoned`
/// keeps only its original meaning, a review that was never started, and is the
/// one state the next tick may ask for again.
const MIGRATION_V34: &str = "
CREATE TABLE review_requests_v34 (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    repository      TEXT NOT NULL,
    pr_number       INTEGER NOT NULL,
    review_type     TEXT NOT NULL,
    head_commit     TEXT NOT NULL,
    backend         TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN (
                        'requested', 'running', 'satisfied', 'failed',
                        'recorded', 'abandoned')),
    detail          TEXT,
    requested_at    INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);

INSERT INTO review_requests_v34 (
    id, repository, pr_number, review_type, head_commit, backend, state,
    detail, requested_at, updated_at
)
SELECT id, repository, pr_number, review_type, head_commit, backend,
       CASE WHEN state = 'abandoned' AND backend = 'record'
            THEN 'recorded' ELSE state END,
       detail, requested_at, updated_at
FROM review_requests;

DROP TABLE review_requests;
ALTER TABLE review_requests_v34 RENAME TO review_requests;

CREATE UNIQUE INDEX review_requests_head
    ON review_requests (repository, pr_number, review_type, head_commit);

CREATE INDEX review_requests_by_pr
    ON review_requests (repository, pr_number);

-- `max_concurrent` is a per-repository budget, so the slot query reads every
-- open request in a repository rather than one pull request's.
CREATE INDEX review_requests_by_repository_state
    ON review_requests (repository, state);
";

/// What the router last saw of each pull request.
///
/// A lifecycle transition is a difference between two looks, so deriving one
/// needs the previous look kept somewhere. The ledger cannot serve: it records
/// what was *asked for*, and a pull request whose policy requests nothing
/// leaves no row at all while still moving through draft, rebases and retargets
/// that a later rule cares about.
///
/// One row per pull request, overwritten in place. This is a memory of the last
/// observation, not a history of them; keeping every look would grow without
/// bound to answer a question only the most recent one can answer.
const MIGRATION_V35: &str = "
CREATE TABLE pull_request_observations (
    repository        TEXT NOT NULL,
    pr_number         INTEGER NOT NULL,
    head_commit       TEXT NOT NULL,
    base_ref          TEXT NOT NULL,
    is_draft          INTEGER NOT NULL,
    state             TEXT NOT NULL,
    dismissed_reviews INTEGER NOT NULL,
    observed_at       INTEGER NOT NULL,
    PRIMARY KEY (repository, pr_number)
);
";

/// The base commit a review was requested against, so "the base moved" becomes
/// observable.
///
/// v35 recorded `base_ref` -- a branch *name*. That answers "was this pull
/// request retargeted", which is a different question from "has the branch it
/// targets advanced since the review ran", and only the second one can make a
/// completed review stale. A timestamp comparison is the usual substitute and
/// is why `Aeptus/mockup` deadlocked on 2026-09-11: a floor derived from clocks
/// called a review stale that had run against the exact head still under
/// consideration. A recorded SHA is the fact itself rather than a proxy for it.
///
/// Nullable on purpose, on both tables. A row written before this migration
/// genuinely has no recorded base, and backfilling one -- from the branch tip
/// today, say -- would assert a comparison nobody made. `freshness =
/// "head_and_base"` reads a missing base as "cannot prove the base is
/// unchanged" and re-requests, which costs one review and never silently
/// passes a stale one.
const MIGRATION_V36: &str = "
ALTER TABLE review_requests ADD COLUMN base_commit TEXT;
ALTER TABLE pull_request_observations ADD COLUMN base_commit TEXT;
";

/// A waived review is its own state, so waiving cannot be read as reviewing.
///
/// Before this, the only way to unblock one stuck dimension was `review state
/// --state satisfied`, which writes the state that means "a verdict landed".
/// The row then claims a review happened. Nobody can later tell a security
/// review that passed from a security review somebody waived at 2am to ship a
/// hotfix, and the distinction is the entire value of the ledger: #172 is
/// exactly this confusion, and the scope of the damage is one dimension per
/// mistake precisely because the ledger is keyed per dimension.
///
/// `waived` is terminal and settles the dimension for one head, no more. The
/// unique index on (repository, pr_number, review_type, head_commit) is what
/// bounds it: a new head has no waived row, so the waiver expires by
/// construction rather than by anyone remembering to withdraw it. There is no
/// repository-wide waiver and no cross-dimension waiver to add later without
/// changing this key, which is the property the issue asks for.
///
/// A full table rebuild rather than an `ALTER`: SQLite cannot widen a CHECK
/// constraint in place, so the v34 rebuild pattern is repeated here -- carrying
/// v36's `base_commit` with it, since the new table must be the current shape
/// and not v34's.
const MIGRATION_V37: &str = "
CREATE TABLE review_requests_v37 (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    repository      TEXT NOT NULL,
    pr_number       INTEGER NOT NULL,
    review_type     TEXT NOT NULL,
    head_commit     TEXT NOT NULL,
    base_commit     TEXT,
    backend         TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN (
                        'requested', 'running', 'satisfied', 'failed',
                        'recorded', 'abandoned', 'waived')),
    detail          TEXT,
    requested_at    INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);

INSERT INTO review_requests_v37 (
    id, repository, pr_number, review_type, head_commit, base_commit, backend,
    state, detail, requested_at, updated_at
)
SELECT id, repository, pr_number, review_type, head_commit, base_commit,
       backend, state, detail, requested_at, updated_at
FROM review_requests;

DROP TABLE review_requests;
ALTER TABLE review_requests_v37 RENAME TO review_requests;

CREATE UNIQUE INDEX review_requests_head
    ON review_requests (repository, pr_number, review_type, head_commit);
";

/// Keep request facts and completion facts independent.
///
/// `head_commit` remains the durable identity column for compatibility with
/// the one-review-per-head index and older readers. New rows also carry the
/// explicit fact columns: requested rows copy their request head into
/// `requested_for_commit`, while an unsolicited completion uses its completed
/// head as the identity and leaves request fields null. This lets a later
/// request for that same head fill in Aethyme's request facts without
/// overwriting what the provider completed.
const MIGRATION_V38: &str = "
CREATE TABLE review_requests_v38 (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    repository            TEXT NOT NULL,
    pr_number             INTEGER NOT NULL,
    review_type           TEXT NOT NULL,
    head_commit           TEXT NOT NULL,
    requested_for_commit  TEXT,
    base_commit           TEXT,
    trigger               TEXT CHECK (trigger IS NULL OR trigger IN (
                              'pull_request_opened', 'ready_for_review',
                              'reopened', 'replacement_commit',
                              'additional_commit', 'base_retargeted',
                              'review_dismissed', 'merge_queue_entered',
                              'scheduled', 'manual', 'unsolicited')),
    backend               TEXT NOT NULL,
    state                 TEXT NOT NULL CHECK (state IN (
                              'requested', 'running', 'satisfied', 'failed',
                              'recorded', 'abandoned', 'waived')),
    detail                TEXT,
    requested_at          INTEGER,
    completed_at          INTEGER,
    completed_for_commit  TEXT,
    verdict               TEXT CHECK (verdict IS NULL OR verdict IN (
                              'pass', 'fail', 'changes_requested', 'commented')),
    reviewer_provider     TEXT CHECK (
                              reviewer_provider IS NULL OR
                              length(trim(reviewer_provider)) > 0),
    reviewer_model        TEXT,
    updated_at            INTEGER NOT NULL,
    CHECK (reviewer_model IS NULL OR reviewer_provider IS NOT NULL),
    CHECK ((completed_at IS NULL AND completed_for_commit IS NULL
             AND verdict IS NULL AND reviewer_provider IS NULL
             AND reviewer_model IS NULL)
           OR (completed_at IS NOT NULL AND completed_for_commit IS NOT NULL
               AND verdict IS NOT NULL AND reviewer_provider IS NOT NULL))
);

INSERT INTO review_requests_v38 (
    id, repository, pr_number, review_type, head_commit,
    requested_for_commit, base_commit, trigger, backend, state, detail,
    requested_at, completed_at, completed_for_commit, verdict,
    reviewer_provider, reviewer_model, updated_at
)
SELECT id, repository, pr_number, review_type, head_commit,
       head_commit, base_commit, NULL, backend, state, detail,
       requested_at, NULL, NULL, NULL, NULL, NULL, updated_at
FROM review_requests;

DROP TABLE review_requests;
ALTER TABLE review_requests_v38 RENAME TO review_requests;

CREATE UNIQUE INDEX review_requests_head
    ON review_requests (repository, pr_number, review_type, head_commit);

CREATE INDEX review_requests_by_pr
    ON review_requests (repository, pr_number);

CREATE INDEX review_requests_by_repository_state
    ON review_requests (repository, state);
";

/// v39 makes broker protection state monotonic without making it permanent:
/// closed-session checkpoint pins get an explicit release ledger, while old
/// publication exposures can reach a terminal expiry state. The exposure
/// rebuild is intentional so the CHECK constraints remain authoritative for
/// databases created before expiry existed.
// `failure_class` arrived in V2 as an ADD COLUMN carrying its own CHECK, and
// SQLite cannot alter a CHECK in place, so admitting `build_failure` means
// rebuilding the table. The copy is a straight column-for-column move: no row
// is reclassified, because nothing recorded before this point distinguished a
// broken build from a failing assertion.
const MIGRATION_V40: &str = "
DROP INDEX IF EXISTS gate_results_by_gate_tree;
DROP INDEX IF EXISTS gate_results_by_gate_tree_definition;
DROP INDEX IF EXISTS gate_results_by_retention_age;
ALTER TABLE gate_results RENAME TO gate_results_v39;

CREATE TABLE gate_results (
    id              INTEGER PRIMARY KEY,
    gate_name       TEXT NOT NULL,
    tree_hash       TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('pass', 'fail', 'cancelled', 'error')),
    exit_code       INTEGER,
    duration_ms     INTEGER,
    log_path        TEXT,
    session_id      INTEGER REFERENCES sessions (id),
    created_at      INTEGER NOT NULL,
    failure_class   TEXT CHECK (failure_class IS NULL OR failure_class IN (
                        'test_failure',
                        'build_failure',
                        'environment',
                        'resource_contention',
                        'timeout',
                        'cached_prior_fail',
                        'unknown'
                    )),
    definition_hash TEXT NOT NULL DEFAULT '',
    wait_duration_ms INTEGER,
    first_output_ms INTEGER,
    output_bytes    INTEGER
);

INSERT INTO gate_results (
    id, gate_name, tree_hash, status, exit_code, duration_ms, log_path,
    session_id, created_at, failure_class, definition_hash, wait_duration_ms,
    first_output_ms, output_bytes
)
SELECT
    id, gate_name, tree_hash, status, exit_code, duration_ms, log_path,
    session_id, created_at, failure_class, definition_hash, wait_duration_ms,
    first_output_ms, output_bytes
FROM gate_results_v39;

DROP TABLE gate_results_v39;

CREATE INDEX gate_results_by_gate_tree ON gate_results (gate_name, tree_hash, id);
CREATE INDEX gate_results_by_gate_tree_definition
    ON gate_results (gate_name, tree_hash, definition_hash, id);
CREATE INDEX gate_results_by_retention_age ON gate_results (created_at, id);
";

const MIGRATION_V39: &str = "
CREATE TABLE gc_checkpoint_pin_releases (
    session_id     INTEGER NOT NULL REFERENCES sessions (id),
    queue_entry_id INTEGER NOT NULL,
    released_at    INTEGER NOT NULL,
    PRIMARY KEY (session_id, queue_entry_id)
);

CREATE INDEX gc_checkpoint_pin_releases_by_queue
    ON gc_checkpoint_pin_releases (queue_entry_id);

DROP INDEX IF EXISTS entry_path_exposures_by_state;
ALTER TABLE entry_path_exposures RENAME TO entry_path_exposures_v38;

CREATE TABLE entry_path_exposures (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    queue_entry_id      INTEGER NOT NULL UNIQUE REFERENCES merge_queue (id),
    promotion_sha       TEXT NOT NULL,
    paths_json          TEXT NOT NULL,
    created_at          INTEGER NOT NULL,
    state               TEXT NOT NULL DEFAULT 'outstanding'
                        CHECK (state IN ('outstanding', 'resolved', 'expired')),
    resolved_at         INTEGER,
    resolution_kind     TEXT CHECK (resolution_kind IS NULL OR resolution_kind IN (
                            'ship_verified', 'external_reconciliation', 'expired'
                        )),
    resolution_sha      TEXT,
    resolution_evidence TEXT
);

INSERT INTO entry_path_exposures (
    id, queue_entry_id, promotion_sha, paths_json, created_at, state,
    resolved_at, resolution_kind, resolution_sha, resolution_evidence
)
SELECT id, queue_entry_id, promotion_sha, paths_json, created_at, state,
       resolved_at, resolution_kind, resolution_sha, resolution_evidence
FROM entry_path_exposures_v38;

DROP TABLE entry_path_exposures_v38;

CREATE INDEX entry_path_exposures_by_state
    ON entry_path_exposures (state, id);
";

const MIGRATION_V41: &str = "
ALTER TABLE sessions ADD COLUMN repository_name TEXT;
ALTER TABLE sessions ADD COLUMN tab_name TEXT;
ALTER TABLE sessions ADD COLUMN ai_provider TEXT;
";
const MIGRATION_V42: &str = "
CREATE TABLE session_scopes (
    id          INTEGER PRIMARY KEY,
    session_id  INTEGER NOT NULL REFERENCES sessions (id),
    kind        TEXT NOT NULL
                CHECK (kind IN ('symbol')),
    value       TEXT NOT NULL,
    operation   TEXT NOT NULL DEFAULT 'unknown'
                CHECK (operation IN ('unknown', 'extend', 'replace', 'remove')),
    source      TEXT NOT NULL
                CHECK (source IN ('declared', 'derived')),
    created_at  INTEGER NOT NULL,
    released_at INTEGER,
    UNIQUE (session_id, kind, value)
);
CREATE INDEX session_scopes_by_target ON session_scopes (kind, value, released_at);
CREATE INDEX session_scopes_by_session ON session_scopes (session_id, released_at);
";
/// Machine conditions per gate run (`GateEnvironment`), so gate-duration
/// trends can be separated from machine load. Purely additive and nullable:
/// declared compatible, so [`MIN_COMPATIBLE_SCHEMA`] stays at 42.
const MIGRATION_V43: &str = "
ALTER TABLE gate_results ADD COLUMN load_avg_1m_start REAL;
ALTER TABLE gate_results ADD COLUMN load_avg_1m_end REAL;
ALTER TABLE gate_results ADD COLUMN cpu_count INTEGER;
ALTER TABLE gate_results ADD COLUMN free_disk_bytes_start INTEGER;
";
const MIGRATION_V44: &str = "
ALTER TABLE sessions ADD COLUMN short_name TEXT;
";
/// Lifecycle telemetry (`crate::insights`).
///
/// Two problems, two tables.
///
/// **Activity intervals.** `sessions.last_activity_at` holds one value — the
/// most recent signal — so every earlier signal is overwritten the moment the
/// agent acts again. Wall-clock session lifetime is therefore the only duration
/// the store can report, and it is not a duration anyone worked: measured over
/// this repository's own history it averages ~46 hours against a median inside
/// the 1-4 hour band, because a session registered on Monday and finished on
/// Thursday reports three days of elapsed time of which the agent spent an
/// unknown and certainly smaller amount working. Intervals keep the history
/// that column threw away.
///
/// The shape is one row per *period of attention*, not per signal: a signal
/// arriving within the idle gap extends the open row, and one arriving after
/// it closes the row at the previous signal and opens a new one. `signals`
/// counts the turns each period absorbed, which is the one engagement number
/// that survives aggregation. The gap that decides the split is a constant in
/// `crate::insights` and is reported alongside every figure computed from
/// these rows, because a duration whose threshold is invisible gets read as a
/// fact about the work rather than a choice about the measurement.
///
/// `ended_at IS NULL` means "still open": the period the session is in right
/// now, which has no duration until the next signal or the session's close
/// ends it. The partial unique index keeps that true under the concurrent
/// hook writes that every session in a repository produces.
///
/// **Pull request milestones.** `pull_request_observations` is a memory of the
/// last look, overwritten in place by design, so it cannot answer "when did
/// this open" — the answer is gone by the second poll, and the provider's own
/// `createdAt` was never requested. Milestones are first-seen facts about a
/// pull request and are immutable once written, which is the opposite shape,
/// so they get their own table keyed by pull request. `session_id` is the
/// session that first observed it and is NULL for a repository whose pull
/// requests nobody watched.
const MIGRATION_V45: &str = "
CREATE TABLE session_activity (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id     INTEGER NOT NULL REFERENCES sessions(id),
    started_at     INTEGER NOT NULL,
    last_signal_at INTEGER NOT NULL,
    ended_at       INTEGER,
    signals        INTEGER NOT NULL DEFAULT 1,
    source         TEXT NOT NULL DEFAULT 'host_hook'
                   CHECK (source IN ('host_hook', 'close'))
);

CREATE UNIQUE INDEX session_activity_one_open
    ON session_activity (session_id) WHERE ended_at IS NULL;

CREATE INDEX session_activity_by_session
    ON session_activity (session_id, started_at);

CREATE TABLE pull_request_milestones (
    repository    TEXT NOT NULL,
    pr_number     INTEGER NOT NULL,
    session_id    INTEGER REFERENCES sessions(id),
    opened_at     INTEGER,
    merged_at     INTEGER,
    closed_at     INTEGER,
    first_seen_at INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    PRIMARY KEY (repository, pr_number)
);

CREATE INDEX pull_request_milestones_by_merge
    ON pull_request_milestones (merged_at)
    WHERE merged_at IS NOT NULL;

CREATE TABLE pull_request_session_links (
    repository    TEXT NOT NULL,
    pr_number     INTEGER NOT NULL,
    session_id    INTEGER NOT NULL REFERENCES sessions(id),
    linked_at     INTEGER NOT NULL,
    link_source   TEXT NOT NULL DEFAULT 'watch'
                   CHECK (link_source IN ('watch', 'representation', 'ship')),
    PRIMARY KEY (repository, pr_number, session_id)
);

CREATE INDEX pull_request_session_links_by_session
    ON pull_request_session_links (session_id);
";
/// Named ownership claims (`crate::ownership`): which session is driving a
/// repository-wide operation such as a release, which no path lease can name.
///
/// History is kept: a release or takeover stamps `released_at` instead of
/// deleting the row, so who held a claim before stays answerable. The partial
/// unique index keeps one open claim per name under concurrent writers.
/// Finishing a session does not touch its rows; every read joins live
/// sessions, the same rule leases and scopes use, so a finished holder's
/// claim stops counting the moment it closes.
const MIGRATION_V46: &str = "
CREATE TABLE ownership_claims (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    name            TEXT NOT NULL,
    session_id      INTEGER NOT NULL REFERENCES sessions(id),
    purpose         TEXT NOT NULL,
    claimed_at      INTEGER NOT NULL,
    released_at     INTEGER,
    released_reason TEXT,
    taken_over_from INTEGER REFERENCES sessions(id)
);

CREATE UNIQUE INDEX ownership_claims_one_open
    ON ownership_claims (name) WHERE released_at IS NULL;

CREATE INDEX ownership_claims_by_session
    ON ownership_claims (session_id, released_at);
";
/// Preserve gate-result history when an operator invalidates a cached verdict,
/// and make row ids monotonic even when retention later physically purges rows.
const MIGRATION_V47: &str = "
DROP INDEX IF EXISTS gate_results_by_gate_tree;
DROP INDEX IF EXISTS gate_results_by_gate_tree_definition;
DROP INDEX IF EXISTS gate_results_by_retention_age;
ALTER TABLE gate_results RENAME TO gate_results_v46;

CREATE TABLE gate_results (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    gate_name       TEXT NOT NULL,
    tree_hash       TEXT NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('pass', 'fail', 'cancelled', 'error')),
    exit_code       INTEGER,
    duration_ms     INTEGER,
    log_path        TEXT,
    session_id      INTEGER REFERENCES sessions (id),
    created_at      INTEGER NOT NULL,
    failure_class   TEXT CHECK (failure_class IS NULL OR failure_class IN (
                        'test_failure',
                        'build_failure',
                        'environment',
                        'resource_contention',
                        'timeout',
                        'cached_prior_fail',
                        'unknown'
                    )),
    definition_hash TEXT NOT NULL DEFAULT '',
    wait_duration_ms INTEGER,
    first_output_ms INTEGER,
    output_bytes    INTEGER,
    load_avg_1m_start REAL,
    load_avg_1m_end   REAL,
    cpu_count        INTEGER,
    free_disk_bytes_start INTEGER,
    cleared_at      INTEGER,
    cleared_reason  TEXT,
    CHECK ((cleared_at IS NULL) = (cleared_reason IS NULL))
);

INSERT INTO gate_results (
    id, gate_name, tree_hash, status, exit_code, duration_ms, log_path,
    session_id, created_at, failure_class, definition_hash, wait_duration_ms,
    first_output_ms, output_bytes, load_avg_1m_start, load_avg_1m_end,
    cpu_count, free_disk_bytes_start, cleared_at, cleared_reason
)
SELECT
    id, gate_name, tree_hash, status, exit_code, duration_ms, log_path,
    session_id, created_at, failure_class, definition_hash, wait_duration_ms,
    first_output_ms, output_bytes, load_avg_1m_start, load_avg_1m_end,
    cpu_count, free_disk_bytes_start, NULL, NULL
FROM gate_results_v46;

DROP TABLE gate_results_v46;

CREATE INDEX gate_results_by_gate_tree ON gate_results (gate_name, tree_hash, id);
CREATE INDEX gate_results_by_gate_tree_definition
    ON gate_results (gate_name, tree_hash, definition_hash, id);
CREATE INDEX gate_results_by_retention_age ON gate_results (created_at, id);

-- Garbage collection is the only authorized physical purge of failing
-- results. Its permit rows live only inside delete_gc_rows' immediate
-- transaction, so an already-open v46 writer cannot use them as a delete path.
CREATE TABLE gate_result_gc_permits (
    gate_result_id INTEGER PRIMARY KEY
);

-- A broker that opened v46 before this migration may still issue its old
-- DELETE. Preserve every failing row, including one it tries to clear before a
-- v47 writer can attach clear metadata, unless GC explicitly permits the row.
CREATE TRIGGER gate_results_preserve_failures_before_delete
BEFORE DELETE ON gate_results
FOR EACH ROW WHEN OLD.status = 'fail'
    AND NOT EXISTS (
        SELECT 1 FROM gate_result_gc_permits WHERE gate_result_id = OLD.id
    )
BEGIN
    SELECT RAISE(ABORT, 'failing gate history is retained');
END;

-- v46 used INTEGER PRIMARY KEY, so a highest row deleted before this rebuild
-- left no sqlite_sequence high-water mark. Gate-result writes append events;
-- `events` uses AUTOINCREMENT and its sequence survives event pruning, so its
-- durable high-water mark bounds gate-result ids even after old rows are gone.
UPDATE sqlite_sequence
SET seq = MAX(
    COALESCE(seq, 0),
    COALESCE((SELECT MAX(id) FROM gate_results), 0),
    COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'events'), 0)
)
WHERE name = 'gate_results';
INSERT INTO sqlite_sequence (name, seq)
SELECT 'gate_results', MAX(
    COALESCE((SELECT MAX(id) FROM gate_results), 0),
    COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'events'), 0)
)
WHERE NOT EXISTS (SELECT 1 FROM sqlite_sequence WHERE name = 'gate_results');
";
const MIGRATION_V48: &str = "
CREATE TABLE repository_watches (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id            INTEGER NOT NULL REFERENCES sessions(id),
    provider              TEXT NOT NULL CHECK (provider IN ('github')),
    canonical_repository  TEXT NOT NULL,
    display_repository    TEXT NOT NULL,
    status                TEXT NOT NULL CHECK (status IN ('active', 'paused', 'stopped')),
    event_kinds_json      TEXT NOT NULL,
    include_drafts        INTEGER NOT NULL CHECK (include_drafts IN (0, 1)),
    exclude_authors_json  TEXT NOT NULL,
    auto_watch            INTEGER NOT NULL CHECK (auto_watch IN (0, 1)),
    poll_interval_seconds INTEGER NOT NULL CHECK (poll_interval_seconds BETWEEN 15 AND 3600),
    last_polled_at        INTEGER,
    next_poll_at          INTEGER,
    last_error_code       TEXT,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
);

CREATE INDEX repository_watches_due
    ON repository_watches (status, next_poll_at, id);

CREATE TABLE repository_watch_pull_requests (
    watch_id      INTEGER NOT NULL REFERENCES repository_watches(id),
    pr_number     INTEGER NOT NULL CHECK (pr_number > 0),
    is_open       INTEGER NOT NULL CHECK (is_open IN (0, 1)),
    is_draft      INTEGER NOT NULL CHECK (is_draft IN (0, 1)),
    first_seen_at INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL,
    PRIMARY KEY (watch_id, pr_number)
);

CREATE TABLE repository_watch_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    watch_id   INTEGER NOT NULL REFERENCES repository_watches(id),
    pr_number  INTEGER NOT NULL CHECK (pr_number > 0),
    kind       TEXT NOT NULL CHECK (kind IN ('opened', 'ready_for_review', 'reopened')),
    title      TEXT NOT NULL,
    author     TEXT,
    url        TEXT,
    head_sha   TEXT NOT NULL,
    is_draft   INTEGER NOT NULL CHECK (is_draft IN (0, 1)),
    created_at INTEGER NOT NULL,
    UNIQUE (watch_id, pr_number, kind)
);

CREATE TABLE repository_delivery_subscriptions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    watch_id   INTEGER NOT NULL REFERENCES repository_watches(id),
    adapter    TEXT NOT NULL,
    target     TEXT NOT NULL,
    policy     TEXT NOT NULL CHECK (policy IN ('notify', 'review')),
    active     INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (watch_id, adapter, target)
);

CREATE TABLE repository_delivery_outbox (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    subscription_id  INTEGER NOT NULL REFERENCES repository_delivery_subscriptions(id),
    event_id         INTEGER NOT NULL REFERENCES repository_watch_events(id),
    status           TEXT NOT NULL CHECK (status IN ('pending', 'claimed', 'delivered', 'failed')),
    generation       INTEGER NOT NULL DEFAULT 0,
    claimed_by       TEXT,
    claim_expires_at INTEGER,
    attempt_count    INTEGER NOT NULL DEFAULT 0,
    last_error_code  TEXT,
    delivered_at     INTEGER,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    UNIQUE (subscription_id, event_id)
);

CREATE INDEX repository_delivery_outbox_due
    ON repository_delivery_outbox (status, claim_expires_at, id);
";
const MIGRATION_V49: &str = "
-- Keep coordinated-operation caller/holder provenance local and nullable so
-- older compatible writers may continue to create history rows.
ALTER TABLE coordinated_operations ADD COLUMN agent_provenance_json TEXT;
";

/// v50 (#660): no schema change; see [`COLLABORATION_FENCE_SCHEMA`]. The
/// statement is a no-op on every database that reaches it.
const MIGRATION_V50: &str = "
-- No schema change: this version marks binaries that implement required
-- collaboration capture, so a repository can fence older ones out.
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
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
    MIGRATION_V17,
    MIGRATION_V18,
    MIGRATION_V19,
    MIGRATION_V20,
    MIGRATION_V21,
    MIGRATION_V22,
    MIGRATION_V23,
    MIGRATION_V24,
    MIGRATION_V25,
    MIGRATION_V26,
    MIGRATION_V27,
    MIGRATION_V28,
    MIGRATION_V29,
    MIGRATION_V30,
    MIGRATION_V31,
    MIGRATION_V32,
    MIGRATION_V33,
    MIGRATION_V34,
    MIGRATION_V35,
    MIGRATION_V36,
    MIGRATION_V37,
    MIGRATION_V38,
    MIGRATION_V39,
    MIGRATION_V40,
    MIGRATION_V41,
    MIGRATION_V42,
    MIGRATION_V43,
    MIGRATION_V44,
    MIGRATION_V45,
    MIGRATION_V46,
    MIGRATION_V47,
    MIGRATION_V48,
    MIGRATION_V49,
    MIGRATION_V50,
];

/// Migrations that only add columns. One is skipped when every column it adds
/// already exists, which a build from an unmerged branch may have done under
/// the same version number (see the module docs).
const COLUMN_ONLY_MIGRATIONS: &[(i64, &[(&str, &str)])] =
    &[(49, &[("coordinated_operations", "agent_provenance_json")])];

/// Migrations whose tables the repair pass re-creates, with `IF NOT EXISTS`,
/// when a database records the version but lacks one of the tables.
const TABLE_MIGRATIONS: &[(i64, &str, &[&str])] = &[(
    48,
    MIGRATION_V48,
    &[
        "repository_watches",
        "repository_watch_pull_requests",
        "repository_watch_events",
        "repository_delivery_subscriptions",
        "repository_delivery_outbox",
    ],
)];

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, BrokerError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        [table, column],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, BrokerError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// `sql` with every `CREATE TABLE` / `CREATE INDEX` made `IF NOT EXISTS`.
fn idempotent_creates(sql: &str) -> String {
    sql.replace("CREATE TABLE IF NOT EXISTS ", "CREATE TABLE ")
        .replace("CREATE UNIQUE INDEX IF NOT EXISTS ", "CREATE UNIQUE INDEX ")
        .replace("CREATE INDEX IF NOT EXISTS ", "CREATE INDEX ")
        .replace("CREATE TABLE ", "CREATE TABLE IF NOT EXISTS ")
        .replace("CREATE UNIQUE INDEX ", "CREATE UNIQUE INDEX IF NOT EXISTS ")
        .replace("CREATE INDEX ", "CREATE INDEX IF NOT EXISTS ")
}

/// Whether the column-only migration `version` has nothing left to add.
fn column_migration_already_applied(conn: &Connection, version: i64) -> Result<bool, BrokerError> {
    let Some((_, columns)) = COLUMN_ONLY_MIGRATIONS.iter().find(|(v, _)| *v == version) else {
        return Ok(false);
    };
    for (table, column) in *columns {
        if !column_exists(conn, table, column)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Objects a recorded migration should have created but the database lacks.
fn missing_recorded_objects(conn: &Connection, version: i64) -> Result<bool, BrokerError> {
    for (v, _, tables) in TABLE_MIGRATIONS {
        if version >= *v {
            for table in *tables {
                if !table_exists(conn, table)? {
                    return Ok(true);
                }
            }
        }
    }
    for (v, columns) in COLUMN_ONLY_MIGRATIONS {
        if version >= *v {
            for (table, column) in *columns {
                if !column_exists(conn, table, column)? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// Re-create the objects of recorded migrations that the database lacks.
/// Checks without a lock first, so a current database pays two cheap reads.
fn repair_recorded_migrations(conn: &Connection) -> Result<(), BrokerError> {
    let version = current_version(conn)?;
    if !missing_recorded_objects(conn, version)? {
        return Ok(());
    }
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let repaired = (|| -> Result<(), BrokerError> {
        let version = current_version(conn)?;
        for (v, sql, _) in TABLE_MIGRATIONS {
            if version >= *v {
                conn.execute_batch(&idempotent_creates(sql))?;
            }
        }
        for (v, columns) in COLUMN_ONLY_MIGRATIONS {
            if version >= *v {
                for (table, column) in *columns {
                    if !column_exists(conn, table, column)? {
                        conn.execute_batch(&format!(
                            "ALTER TABLE {table} ADD COLUMN {column} TEXT"
                        ))?;
                    }
                }
            }
        }
        Ok(())
    })();
    match repaired {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(err) => {
            // Report the repair error unless the rollback itself fails.
            return conn
                .execute_batch("ROLLBACK")
                .map_err(BrokerError::from)
                .and(Err(err));
        }
    }
    Ok(())
}

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
        if newer_schema_is_compatible(conn, found)? {
            return Ok(());
        }
        return Err(BrokerError::SchemaTooNew {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    if found == SCHEMA_VERSION {
        repair_recorded_migrations(conn)?;
        record_min_compatible_schema(conn)?;
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
        let applied = (|| -> Result<(), BrokerError> {
            if !column_migration_already_applied(conn, version)? {
                conn.execute_batch(sql)?;
            }
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [version.to_string()],
            )?;
            if version >= MIN_COMPATIBLE_SCHEMA {
                record_min_compatible_schema(conn)?;
            }
            Ok(())
        })();
        match applied {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(err) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(err);
            }
        }
    }
    repair_recorded_migrations(conn)?;
    record_min_compatible_schema(conn)?;
    Ok(())
}

/// Record [`MIN_COMPATIBLE_SCHEMA`], never lowering a value a newer binary
/// already wrote: that binary knows about migrations this one does not.
fn record_min_compatible_schema(conn: &Connection) -> Result<(), BrokerError> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('min_compatible_schema', ?1)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value
         WHERE CAST(meta.value AS INTEGER) < CAST(excluded.value AS INTEGER)",
        [MIN_COMPATIBLE_SCHEMA.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrated() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    fn set_meta(conn: &Connection, key: &str, value: i64) {
        conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value.to_string()],
        )
        .unwrap();
    }

    #[test]
    fn migrate_records_the_min_compatible_schema() {
        let conn = migrated();
        let value: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'min_compatible_schema'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, MIN_COMPATIBLE_SCHEMA.to_string());
    }

    #[test]
    fn an_older_binary_opens_a_newer_database_only_when_declared_compatible() {
        // A newer binary applied an additive migration and kept the minimum.
        let conn = migrated();
        set_meta(&conn, "schema_version", SCHEMA_VERSION + 1);
        assert!(migrate(&conn).is_ok(), "additive newer schema must open");
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION + 1);

        // A newer binary declared a breaking migration.
        set_meta(&conn, "min_compatible_schema", SCHEMA_VERSION + 1);
        assert!(matches!(
            migrate(&conn),
            Err(BrokerError::SchemaTooNew { .. })
        ));

        // A newer database from before the marker existed is refused.
        conn.execute("DELETE FROM meta WHERE key = 'min_compatible_schema'", [])
            .unwrap();
        assert!(matches!(
            migrate(&conn),
            Err(BrokerError::SchemaTooNew { .. })
        ));
    }

    /// The fence constant names migration v50, so a renumbering rebase that
    /// moves the migration without the constant fails here.
    #[test]
    fn the_collaboration_fence_is_the_version_of_its_marker_migration() {
        let index = usize::try_from(COLLABORATION_FENCE_SCHEMA - 1).unwrap();
        assert_eq!(MIGRATIONS[index], MIGRATION_V50);
        const { assert!(SCHEMA_VERSION >= COLLABORATION_FENCE_SCHEMA) };
        const { assert!(MIN_COMPATIBLE_SCHEMA < COLLABORATION_FENCE_SCHEMA) };
    }

    /// Without the fence a pre-#660 binary (schema 49) still opens a v50
    /// database; with it, that binary is refused and this one is not.
    #[test]
    fn the_collaboration_fence_refuses_older_binaries_and_is_never_lowered() {
        let conn = migrated();
        let found = current_version(&conn).unwrap();
        assert!(schema_is_compatible_with(&conn, found, 49).unwrap());
        assert_eq!(collaboration_fence(&conn).unwrap(), None);

        raise_collaboration_fence(&conn, "committed").unwrap();
        assert!(!schema_is_compatible_with(&conn, found, 49).unwrap());
        assert!(schema_is_compatible_with(&conn, found, COLLABORATION_FENCE_SCHEMA).unwrap());
        assert_eq!(
            collaboration_fence(&conn).unwrap(),
            Some(CollaborationFence {
                min_compatible_schema: COLLABORATION_FENCE_SCHEMA,
                reason: COLLABORATION_FENCE_REASON.into(),
                source: "committed".into(),
                state: "active",
            })
        );
        // Re-opening, re-raising and re-migrating never lower it.
        raise_collaboration_fence(&conn, "committed").unwrap();
        migrate(&conn).unwrap();
        record_min_compatible_schema(&conn).unwrap();
        assert!(!schema_is_compatible_with(&conn, found, 49).unwrap());

        // A higher floor from a later migration is kept.
        set_meta(
            &conn,
            "min_compatible_schema",
            COLLABORATION_FENCE_SCHEMA + 3,
        );
        raise_collaboration_fence(&conn, "committed").unwrap();
        assert_eq!(
            collaboration_fence(&conn)
                .unwrap()
                .unwrap()
                .min_compatible_schema,
            COLLABORATION_FENCE_SCHEMA + 3
        );
    }

    /// A raise that fails part-way rolls back and leaves no transaction open,
    /// so the connection keeps working and other processes are not blocked.
    #[test]
    fn a_failed_fence_raise_leaves_no_open_transaction() {
        let conn = migrated();
        conn.execute_batch(
            "CREATE TRIGGER refuse_fence_source BEFORE INSERT ON meta
             WHEN NEW.key = 'collaboration_fence_source'
             BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .unwrap();
        assert!(raise_collaboration_fence(&conn, "committed").is_err());
        assert!(conn.is_autocommit(), "a transaction was left open");
        // The floor was rolled back with the rest.
        assert_eq!(collaboration_fence(&conn).unwrap(), None);
        let found = current_version(&conn).unwrap();
        assert!(schema_is_compatible_with(&conn, found, 49).unwrap());
        conn.execute_batch("BEGIN IMMEDIATE; COMMIT;").unwrap();
    }

    #[test]
    fn the_min_compatible_marker_is_never_lowered() {
        let conn = migrated();
        set_meta(&conn, "min_compatible_schema", MIN_COMPATIBLE_SCHEMA + 5);
        record_min_compatible_schema(&conn).unwrap();
        let value: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'min_compatible_schema'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, (MIN_COMPATIBLE_SCHEMA + 5).to_string());
    }

    /// Apply migrations 1..=`through` the way `migrate` does, without the
    /// min-compatible bookkeeping, to build a database an older binary left.
    fn migrate_through(conn: &Connection, through: usize) {
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS.iter().take(through).enumerate() {
            conn.execute_batch(sql).unwrap();
            set_meta(conn, "schema_version", (index + 1) as i64);
        }
    }

    fn migrated_through(through: usize) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate_through(&conn, through);
        conn
    }

    /// The exact insert a v42 binary (0.8.2-0.8.6) issues for a gate result: it
    /// names its columns and knows nothing of the v43 environment columns.
    const V42_GATE_RESULT_INSERT: &str = "INSERT INTO gate_results (gate_name, tree_hash,
             definition_hash, status, failure_class, exit_code, duration_ms, log_path,
             session_id, created_at, wait_duration_ms, first_output_ms, output_bytes)
         VALUES (?1, 'tree', 'def', 'pass', NULL, 0, 1200, NULL, NULL, ?2, 0, 5, 10)";

    #[test]
    fn v47_minimum_compatible_schema_is_committed_atomically() {
        let conn = migrated_through(46);
        set_meta(&conn, "min_compatible_schema", 42);
        conn.execute_batch(
            "CREATE TRIGGER refuse_min_compatible_raise
             BEFORE UPDATE OF value ON meta
             WHEN OLD.key = 'min_compatible_schema'
                  AND CAST(NEW.value AS INTEGER) > CAST(OLD.value AS INTEGER)
             BEGIN
                 SELECT RAISE(ABORT, 'injected compatibility-floor failure');
             END;",
        )
        .unwrap();

        assert!(
            migrate(&conn).is_err(),
            "the injected floor write must fail"
        );
        assert_eq!(current_version(&conn).unwrap(), 46);
        let minimum: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'min_compatible_schema'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(minimum, "42");
        let columns = conn
            .prepare("PRAGMA table_info(gate_results)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(!columns.iter().any(|column| column == "cleared_at"));
    }

    #[test]
    fn an_already_open_v46_connection_cannot_delete_cleared_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("broker.db");
        let old_connection = Connection::open(&path).unwrap();
        migrate_through(&old_connection, 46);
        old_connection
            .execute(V42_GATE_RESULT_INSERT, rusqlite::params!["old-failure", 1])
            .unwrap();
        old_connection
            .execute(
                "UPDATE gate_results SET status = 'fail', failure_class = 'test_failure'
                 WHERE id = 1",
                [],
            )
            .unwrap();

        let migrator = Connection::open(&path).unwrap();
        migrate(&migrator).unwrap();
        let deletion_before_clear =
            old_connection.execute("DELETE FROM gate_results WHERE id = 1", []);
        assert!(
            deletion_before_clear.is_err(),
            "a pre-migration writer must not erase an unmarked failure"
        );
        migrator
            .execute(
                "UPDATE gate_results SET cleared_at = 2, cleared_reason = 'operator review'
                 WHERE id = 1",
                [],
            )
            .unwrap();

        let deletion = old_connection.execute("DELETE FROM gate_results WHERE id = 1", []);
        assert!(
            deletion.is_err(),
            "a pre-migration writer must not erase a cleared result"
        );
        let reason: String = old_connection
            .query_row(
                "SELECT cleared_reason FROM gate_results WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reason, "operator review");
    }

    #[test]
    fn v43_adds_nullable_gate_environment_columns_to_existing_rows() {
        let conn = migrated_through(42);
        conn.execute(V42_GATE_RESULT_INSERT, rusqlite::params!["before", 1])
            .unwrap();

        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);

        // The row written before the migration keeps its data and has no
        // environment: NULL, never a fabricated zero.
        let (duration, load_start, load_end, cpus, free): (
            i64,
            Option<f64>,
            Option<f64>,
            Option<i64>,
            Option<i64>,
        ) = conn
            .query_row(
                "SELECT duration_ms, load_avg_1m_start, load_avg_1m_end, cpu_count,
                        free_disk_bytes_start
                 FROM gate_results WHERE gate_name = 'before'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(duration, 1200);
        assert_eq!((load_start, load_end, cpus, free), (None, None, None, None));

        // A row written after it carries values.
        conn.execute(
            "INSERT INTO gate_results (gate_name, tree_hash, definition_hash, status,
                 created_at, load_avg_1m_start, load_avg_1m_end, cpu_count,
                 free_disk_bytes_start)
             VALUES ('after', 'tree', 'def', 'pass', 2, 12.5, 9.25, 10, 25000000000)",
            [],
        )
        .unwrap();
        let (load_start, load_end, cpus, free): (f64, f64, i64, i64) = conn
            .query_row(
                "SELECT load_avg_1m_start, load_avg_1m_end, cpu_count, free_disk_bytes_start
                 FROM gate_results WHERE gate_name = 'after'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (load_start, load_end, cpus, free),
            (12.5, 9.25, 10, 25_000_000_000)
        );
    }

    #[test]
    fn v47_preserves_gate_history_and_never_reuses_gate_result_ids() {
        let conn = migrated_through(46);
        conn.execute(
            "INSERT INTO gate_results (
                 id, gate_name, tree_hash, definition_hash, status, failure_class,
                 exit_code, duration_ms, created_at
             ) VALUES (
                 1506, 'unit', 'tree', 'definition', 'fail', 'test_failure', 1, 1200, 7
             )",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO gate_results (
                 id, gate_name, tree_hash, definition_hash, status, failure_class,
                 exit_code, duration_ms, created_at
             ) VALUES (
                 1507, 'unit', 'tree', 'definition', 'pass', NULL, 0, 10, 8
             )",
            [],
        )
        .unwrap();
        conn.execute("DELETE FROM gate_results WHERE id = 1507", [])
            .unwrap();
        conn.execute(
            "INSERT INTO events (id, schema_version, ts, kind, session_id, payload_json)
             VALUES (1507, ?1, 1507, 'gate.fail', NULL, '{}')",
            [EVENTS_SCHEMA_VERSION],
        )
        .unwrap();
        conn.execute("DELETE FROM events", []).unwrap();

        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
        let (gate, tree, status, failure_class, duration, cleared_at, cleared_reason): (
            String,
            String,
            String,
            Option<String>,
            i64,
            Option<i64>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT gate_name, tree_hash, status, failure_class, duration_ms,
                        cleared_at, cleared_reason
                 FROM gate_results WHERE id = 1506",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            (
                gate.as_str(),
                tree.as_str(),
                status.as_str(),
                failure_class.as_deref(),
                duration
            ),
            ("unit", "tree", "fail", Some("test_failure"), 1200)
        );
        assert_eq!((cleared_at, cleared_reason), (None, None));

        conn.execute(
            "INSERT INTO gate_results (gate_name, tree_hash, definition_hash, status, created_at)
             VALUES ('unit', 'tree', 'definition', 'pass', 8)",
            [],
        )
        .unwrap();
        let purged_id = conn.last_insert_rowid();
        assert!(
            purged_id > 1507,
            "pruned event ids still bound the gate-result high-water mark"
        );
        conn.execute("DELETE FROM gate_results WHERE id = ?1", [purged_id])
            .unwrap();
        conn.execute(
            "INSERT INTO gate_results (gate_name, tree_hash, definition_hash, status, created_at)
             VALUES ('unit', 'tree', 'definition', 'pass', 9)",
            [],
        )
        .unwrap();
        assert!(
            conn.last_insert_rowid() > purged_id,
            "a purged gate result id must never be reused"
        );
    }

    #[test]
    fn v47_requires_a_v47_reader_to_preserve_cleared_gate_history() {
        // v48 is additive (#606), so the floor stays at the v47 reader.
        assert_eq!(MIN_COMPATIBLE_SCHEMA, 47);

        let conn = migrated();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
        assert!(schema_is_compatible_with(&conn, SCHEMA_VERSION, 47).unwrap());
        assert!(!schema_is_compatible_with(&conn, SCHEMA_VERSION, 46).unwrap());
        assert!(!schema_is_compatible_with(&conn, SCHEMA_VERSION, 42).unwrap());
    }

    #[test]
    fn v48_only_adds_repository_watch_tables_and_keeps_v47_readers_compatible() {
        let before_conn = migrated_through(47);
        let schema = |conn: &Connection| -> Vec<(String, String)> {
            let mut statement = conn
                .prepare("SELECT name, sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")
                .unwrap();
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let before = schema(&before_conn);
        let conn = migrated_through(48);
        // A v48 binary stamps the compatibility floor when it finishes
        // migrating; `migrated_through` stops before that step.
        set_meta(&conn, "min_compatible_schema", 47);
        assert_eq!(current_version(&conn).unwrap(), 48);
        let after = schema(&conn);
        for object in &before {
            assert!(after.contains(object), "v48 changed {}", object.0);
        }
        let added: Vec<&str> = after
            .iter()
            .filter(|object| !before.contains(object))
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(
            added,
            vec![
                "repository_delivery_outbox",
                "repository_delivery_outbox_due",
                "repository_delivery_subscriptions",
                "repository_watch_events",
                "repository_watch_pull_requests",
                "repository_watches",
                "repository_watches_due",
            ]
        );
        assert_eq!(MIN_COMPATIBLE_SCHEMA, 47);
        assert!(schema_is_compatible_with(&conn, 48, 47).unwrap());
    }

    #[test]
    fn v49_adds_agent_provenance_without_raising_the_compatibility_floor() {
        const { assert!(SCHEMA_VERSION >= 49) };
        assert_eq!(MIN_COMPATIBLE_SCHEMA, 47);

        let conn = migrated_through(48);
        assert_eq!(current_version(&conn).unwrap(), 48);
        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
        assert!(schema_is_compatible_with(&conn, SCHEMA_VERSION, 47).unwrap());
        assert!(!schema_is_compatible_with(&conn, SCHEMA_VERSION, 46).unwrap());

        let nullable: i64 = conn
            .query_row(
                r#"SELECT "notnull" FROM pragma_table_info('coordinated_operations')
                   WHERE name = 'agent_provenance_json'"#,
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(nullable, 0);

        // A v47 database, the compatibility floor, migrates straight through.
        let from_v47 = migrated_through(47);
        migrate(&from_v47).unwrap();
        assert_eq!(current_version(&from_v47).unwrap(), SCHEMA_VERSION);
        assert!(schema_is_compatible_with(&from_v47, SCHEMA_VERSION, 47).unwrap());
    }

    /// v50 changes nothing: a v49 database migrates to it, the floor stays at
    /// 47, and a v49 binary still opens the result. Only the collaboration
    /// fence, raised per repository, shuts v49 binaries out.
    #[test]
    fn v50_is_a_compatible_marker_that_leaves_the_floor_alone() {
        assert_eq!(SCHEMA_VERSION, 50);
        let conn = migrated_through(49);
        let tables: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
            .unwrap();
        migrate(&conn).unwrap();
        assert_eq!(current_version(&conn).unwrap(), 50);
        let after: i64 = conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after, tables, "v50 must not change the schema");
        assert!(schema_is_compatible_with(&conn, 50, 49).unwrap());
        assert!(schema_is_compatible_with(&conn, 50, 47).unwrap());
        assert_eq!(collaboration_fence(&conn).unwrap(), None);
    }

    #[test]
    fn additive_v44_is_declared_compatible_for_a_v42_writer() {
        let conn = migrated_through(44);
        set_meta(&conn, "min_compatible_schema", 42);
        assert_eq!(current_version(&conn).unwrap(), 44);
        assert!(schema_is_compatible_with(&conn, 44, 42).unwrap());
        assert!(!schema_is_compatible_with(&conn, 44, 41).unwrap());

        conn.execute(V42_GATE_RESULT_INSERT, rusqlite::params!["old-writer", 3])
            .unwrap();
        let cpus: Option<i64> = conn
            .query_row(
                "SELECT cpu_count FROM gate_results WHERE gate_name = 'old-writer'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cpus, None);
    }

    #[test]
    fn additive_v45_is_declared_compatible_for_a_v42_writer() {
        let conn = migrated_through(45);
        set_meta(&conn, "min_compatible_schema", 42);
        assert_eq!(current_version(&conn).unwrap(), 45);
        assert!(schema_is_compatible_with(&conn, 45, 42).unwrap());
        assert!(!schema_is_compatible_with(&conn, 45, 41).unwrap());

        conn.execute(V42_GATE_RESULT_INSERT, rusqlite::params!["old-writer", 3])
            .unwrap();
        let cpus: Option<i64> = conn
            .query_row(
                "SELECT cpu_count FROM gate_results WHERE gate_name = 'old-writer'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cpus, None);
    }

    #[test]
    fn a_name_has_at_most_one_open_ownership_claim() {
        // The active binary is compatible with the migrated schema; a v46
        // binary is intentionally refused because its unblock deletes history.
        let conn = migrated();
        assert!(schema_is_compatible_with(&conn, 47, 47).unwrap());
        assert!(!schema_is_compatible_with(&conn, 47, 46).unwrap());
        for id in [1, 2] {
            conn.execute(
                "INSERT INTO sessions (
                     id, worktree_path, branch, origin, status,
                     created_at, updated_at, last_activity_at
                 ) VALUES (?1, '/repo/' || ?1, 'agent/x' || ?1, 'spawned', 'active', 1, 1, 1)",
                [id],
            )
            .unwrap();
        }
        let claim = |session: i64| {
            conn.execute(
                "INSERT INTO ownership_claims (name, session_id, purpose, claimed_at)
                 VALUES ('release v1', ?1, 'cut v1', 1)",
                [session],
            )
        };
        claim(1).unwrap();
        assert!(claim(2).is_err());
        conn.execute(
            "UPDATE ownership_claims SET released_at = 2 WHERE session_id = 1",
            [],
        )
        .unwrap();
        claim(2).unwrap();
    }

    #[test]
    fn a_session_has_at_most_one_open_activity_interval() {
        // The invariant the partial unique index exists to hold: concurrent
        // hook writes from every session in a repository must not open two
        // periods of attention for one session.
        let conn = migrated();
        conn.execute(
            "INSERT INTO sessions (
                 id, worktree_path, branch, origin, status,
                 created_at, updated_at, last_activity_at
             ) VALUES (1, '/repo', 'agent/one', 'spawned', 'active', 1, 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_activity (session_id, started_at, last_signal_at)
             VALUES (1, 100, 100)",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO session_activity (session_id, started_at, last_signal_at)
                 VALUES (1, 200, 200)",
                [],
            )
            .is_err(),
            "a second open interval for the same session must be refused"
        );
        // A closed interval is history, not a conflict.
        conn.execute(
            "UPDATE session_activity SET ended_at = 150 WHERE session_id = 1",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_activity (session_id, started_at, last_signal_at)
             VALUES (1, 200, 200)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn a_pull_request_milestone_is_written_once_per_pull_request() {
        // First-seen facts are immutable, so the key is the pull request
        // itself: a second observer of the same pull request updates the
        // existing row's facts rather than adding a competing one.
        let conn = migrated();
        conn.execute(
            "INSERT INTO pull_request_milestones (repository, pr_number, opened_at, first_seen_at, updated_at)
             VALUES ('o/r', 7, 1000, 1000, 1000)",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "INSERT INTO pull_request_milestones (repository, pr_number, opened_at, first_seen_at, updated_at)
                 VALUES ('o/r', 7, 2000, 2000, 2000)",
                [],
            )
            .is_err(),
            "a pull request must have one milestone row"
        );
    }

    #[test]
    fn a_pull_request_links_to_several_sessions() {
        // Two sessions can each open a watch on one pull request — a session
        // that created it and one that was later assigned its review — so the
        // link table is many-to-many and the primary key allows it. Without
        // that, the second link is silently dropped and the pull request looks
        // like it belongs to whoever saw it first.
        let conn = migrated();
        for (id, path) in [(1i64, "/one"), (2, "/two")] {
            conn.execute(
                "INSERT INTO sessions (
                     id, worktree_path, branch, origin, status,
                     created_at, updated_at, last_activity_at
                 ) VALUES (?1, ?2, 'agent/x', 'spawned', 'active', 1, 1, 1)",
                rusqlite::params![id, path],
            )
            .unwrap();
        }
        for session in [1i64, 2] {
            conn.execute(
                "INSERT INTO pull_request_session_links (repository, pr_number, session_id, linked_at)
                 VALUES ('o/r', 7, ?1, 10)",
                rusqlite::params![session],
            )
            .unwrap();
        }
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pull_request_session_links WHERE repository='o/r' AND pr_number=7",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn v40_preserves_gate_results_and_accepts_build_failure() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS.iter().take(39).enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO gate_results (
                 id, gate_name, tree_hash, status, exit_code, duration_ms,
                 log_path, session_id, created_at, failure_class,
                 definition_hash, wait_duration_ms, first_output_ms, output_bytes
             ) VALUES (
                 7, 'cargo-test', 'e833c8c3', 'fail', 101, 24281,
                 '/logs/cargo-test.log', NULL, 1700, 'test_failure',
                 'definition', 76, NULL, 1185
             )",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "UPDATE gate_results SET failure_class = 'build_failure' WHERE id = 7",
                [],
            )
            .is_err(),
            "the old CHECK is what forces the rebuild"
        );

        conn.execute_batch(MIGRATIONS[39]).unwrap();

        let row: (String, String, i64, String, i64) = conn
            .query_row(
                "SELECT gate_name, status, exit_code, failure_class, output_bytes
                 FROM gate_results WHERE id = 7",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "cargo-test".to_string(),
                "fail".to_string(),
                101,
                "test_failure".to_string(),
                1185
            ),
            "the copy must carry every column and reclassify nothing"
        );

        let indexes: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type = 'index' AND tbl_name = 'gate_results' AND name IN (
                     'gate_results_by_gate_tree',
                     'gate_results_by_gate_tree_definition',
                     'gate_results_by_retention_age'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            indexes, 3,
            "the rebuild must restore every index it dropped"
        );

        conn.execute(
            "UPDATE gate_results SET failure_class = 'build_failure' WHERE id = 7",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "UPDATE gate_results SET failure_class = 'not_a_class' WHERE id = 7",
                [],
            )
            .is_err(),
            "widening the CHECK must not stop it rejecting unknown values"
        );
    }

    #[test]
    fn v42_adds_session_scopes_without_disturbing_existing_sessions() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS.iter().take(41).enumerate() {
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
             ) VALUES ('/repo/worktree', 'agent/legacy', 'adopted', 'active',
                       'legacy task', 'base', 1, 1, 1)",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        // The session that predates the migration is untouched.
        let task: String = conn
            .query_row("SELECT task FROM sessions WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(task, "legacy task");

        // A target can be recorded against it.
        conn.execute(
            "INSERT INTO session_scopes
                 (session_id, kind, value, operation, source, created_at)
             VALUES (1, 'symbol', 'PaymentService', 'replace', 'declared', 1)",
            [],
        )
        .unwrap();

        // One row per (session, kind, value): re-recording a target updates it
        // rather than accumulating duplicates that would pair with themselves.
        conn.execute(
            "INSERT INTO session_scopes
                 (session_id, kind, value, operation, source, created_at)
             VALUES (1, 'symbol', 'PaymentService', 'extend', 'derived', 2)
             ON CONFLICT (session_id, kind, value) DO UPDATE SET
                 operation = excluded.operation",
            [],
        )
        .unwrap();
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM session_scopes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1, "the target must not be recorded twice");

        // The stored strings are the contract: an operation outside the
        // vocabulary is refused rather than silently kept.
        let refused = conn.execute(
            "INSERT INTO session_scopes
                 (session_id, kind, value, operation, source, created_at)
             VALUES (1, 'symbol', 'Other', 'rewrite', 'declared', 3)",
            [],
        );
        assert!(refused.is_err(), "unknown operation must be refused");

        let refused_kind = conn.execute(
            "INSERT INTO session_scopes
                 (session_id, kind, value, operation, source, created_at)
             VALUES (1, 'ledger', 'Other', 'extend', 'declared', 3)",
            [],
        );
        assert!(refused_kind.is_err(), "unknown kind must be refused");
    }

    #[test]
    fn v41_adds_session_context_columns_without_changing_existing_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS.iter().take(40).enumerate() {
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
             ) VALUES ('/repo/worktree', 'agent/legacy', 'adopted', 'active',
                       'legacy task', 'base', 1, 1, 1)",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let columns = conn
            .prepare("PRAGMA table_info(sessions)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for expected in ["repository_name", "tab_name", "ai_provider", "short_name"] {
            assert!(
                columns.iter().any(|column| column == expected),
                "{expected}"
            );
        }
        let context: (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT repository_name, tab_name, ai_provider, short_name
                 FROM sessions WHERE branch = 'agent/legacy'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(context, (None, None, None, None));
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v30_preserves_advisories_and_accepts_explicit_suppression() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS.iter().take(29).enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO advisories (
                 identity, audience, producer, session_id, severity,
                 queue_entry_id, integration_sha, paths_json, evidence_json,
                 created_at, resolution_state
             ) VALUES (
                 'history:test', 'maintainer', 'conflict_history', NULL,
                 'warning', NULL, NULL, '[]', '[]', 1, 'acknowledged'
             )",
            [],
        )
        .unwrap();
        let advisory_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO advisory_delivery_metrics (
                 advisory_id, surface, first_shown_at, last_shown_at,
                 show_count, acted_at, action
             ) VALUES (?1, 'inventory', 1, 1, 1, 1, 'acknowledged')",
            [advisory_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO external_coordination_events (
                 provider, provider_event_id, event_type, repository,
                 target_branch, pr_number, commit_sha, occurred_at,
                 verification_method, verified_at, normalized_digest,
                 status, advisory_id, received_at
             ) VALUES (
                 'github', 'event-1', 'review', 'owner/repo', 'main', 1,
                 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1,
                 'authenticated_poll', 1, 'digest', 'advisory_created', ?1, 1
             )",
            [advisory_id],
        )
        .unwrap();
        migrate(&conn).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT audience || ':' || producer || ':' || resolution_state
                 FROM advisories WHERE identity = 'history:test'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "maintainer:conflict_history:acknowledged"
        );
        conn.execute(
            "UPDATE advisories SET suppressed_at = 2
             WHERE identity = 'history:test'",
            [],
        )
        .unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT resolution_state || ':' || suppressed_at
                 FROM advisories WHERE identity = 'history:test'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "acknowledged:2"
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM external_coordination_events
                 WHERE advisory_id = ?1",
                [advisory_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            0
        );
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

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
    fn v17_adds_entry_exposures_and_preserves_v16_advisories() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..16].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO advisories (
                 identity, severity, paths_json, evidence_json, created_at,
                 resolution_state, acknowledged_at
             ) VALUES ('legacy-advisory', 'warning', '[]', '[]', 1,
                       'acknowledged', 2)",
            [],
        )
        .unwrap();

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
            "resolved_at",
            "resolution_evidence",
        ] {
            assert!(
                columns.iter().any(|column| column == expected),
                "{expected}"
            );
        }
        let exposure_columns = conn
            .prepare("PRAGMA table_info(entry_path_exposures)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for expected in [
            "queue_entry_id",
            "promotion_sha",
            "paths_json",
            "state",
            "resolved_at",
            "resolution_kind",
            "resolution_sha",
            "resolution_evidence",
        ] {
            assert!(
                exposure_columns.iter().any(|column| column == expected),
                "{expected}"
            );
        }
        let preserved = conn
            .query_row(
                "SELECT resolution_state, acknowledged_at, resolved_at
                 FROM advisories WHERE identity = 'legacy-advisory'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(preserved, ("acknowledged".into(), Some(2), None));
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
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

    #[test]
    fn v18_adds_recipient_indexed_session_notes_without_rewriting_history() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..17].iter().enumerate() {
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
            .prepare("PRAGMA table_info(session_notes)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            columns,
            [
                "id",
                "sender_session_id",
                "recipient_session_id",
                "message",
                "created_at",
                "acknowledged_at",
            ]
        );
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v19_separates_logical_closure_from_physical_cleanup() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..18].iter().enumerate() {
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
                 id, worktree_path, branch, origin, status,
                 created_at, updated_at, last_activity_at
             ) VALUES (1, '/tmp/retained', 'agent/retained', 'spawned', 'cleaned', 10, 20, 10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (
                 id, worktree_path, branch, origin, status,
                 created_at, updated_at, last_activity_at
             ) VALUES (2, '/tmp/live', 'agent/live', 'spawned', 'active', 10, 20, 10)",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let retained: (String, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT cleanup_state, closed_at, cleanup_completed_at FROM sessions WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(retained, ("closed".into(), Some(20), None));
        let live: (String, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT cleanup_state, closed_at, cleanup_completed_at FROM sessions WHERE id = 2",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(live, ("open".into(), None, None));
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v20_indexes_every_retention_age_walk() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        for (table, expected) in [
            ("events", "events_by_retention_age"),
            ("gate_results", "gate_results_by_retention_age"),
            ("merge_queue", "merge_queue_by_retention_status_age"),
            ("sessions", "sessions_by_cleanup_age"),
        ] {
            let mut statement = conn
                .prepare(&format!("PRAGMA index_list('{table}')"))
                .unwrap();
            let indexes = statement
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert!(indexes.iter().any(|index| index == expected), "{table}");
        }
    }

    #[test]
    fn v21_indexes_latest_queue_entry_by_session() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let indexes = conn
            .prepare("PRAGMA index_list('merge_queue')")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            indexes
                .iter()
                .any(|index| index == "merge_queue_by_session_id")
        );
    }

    #[test]
    fn v22_adds_bounded_advisory_delivery_metrics() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'advisory_delivery_metrics'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(sql.contains("PRIMARY KEY (advisory_id, surface)"));
        let columns = conn
            .prepare("PRAGMA table_info(advisory_delivery_metrics)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            columns,
            [
                "advisory_id",
                "session_id",
                "surface",
                "first_shown_at",
                "last_shown_at",
                "show_count",
                "acted_at",
                "action",
            ]
        );
    }

    #[test]
    fn v23_adds_redacted_external_event_storage_without_rewriting_history() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..22].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO events (schema_version, ts, kind) VALUES (1, 10, 'legacy')",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let columns = conn
            .prepare("PRAGMA table_info(external_coordination_events)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for expected in [
            "provider_event_id",
            "normalized_digest",
            "status",
            "session_id",
            "advisory_id",
            "reconciliation_reason_digest",
        ] {
            assert!(
                columns.iter().any(|column| column == expected),
                "{expected}"
            );
        }
        for forbidden in ["payload", "body", "comment", "diff", "credential", "task"] {
            assert!(
                columns.iter().all(|column| !column.contains(forbidden)),
                "forbidden storage column {forbidden}"
            );
        }
        assert_eq!(
            conn.query_row("SELECT kind FROM events WHERE id = 1", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
            "legacy"
        );
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v24_adds_review_provenance_and_transition_history() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..23].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }

        migrate(&conn).unwrap();

        let lifecycle_columns = conn
            .prepare("PRAGMA table_info(review_lifecycles)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for expected in [
            "session_id",
            "queue_entry_id",
            "repository",
            "target_branch",
            "pr_number",
            "commit_sha",
            "state",
            "generation",
            "evidence_digest",
            "unlock_operation_id",
        ] {
            assert!(lifecycle_columns.iter().any(|column| column == expected));
        }
        let transition_columns = conn
            .prepare("PRAGMA table_info(review_lifecycle_transitions)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for expected in ["from_state", "to_state", "commit_sha", "operation_id"] {
            assert!(transition_columns.iter().any(|column| column == expected));
        }
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v25_preserves_review_history_and_makes_only_active_owners_unique() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..24].iter().enumerate() {
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
                 id, worktree_path, branch, origin, status,
                 created_at, updated_at, last_activity_at
             ) VALUES (1, '/tmp/old', 'agent/old', 'spawned', 'cleaned', 1, 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO review_lifecycles (
                 session_id, repository, target_branch, pr_number, commit_sha,
                 state, generation, created_at, updated_at
             ) VALUES (1, 'github.com/acme/product', 'main', 42,
                       'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                       'review_requested', 3, 1, 1)",
            [],
        )
        .unwrap();
        let lifecycle_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO review_lifecycle_transitions (
                 lifecycle_id, from_state, to_state, commit_sha, created_at
             ) VALUES (?1, 'local_submission_verified', 'review_requested',
                       'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1)",
            [lifecycle_id],
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert_eq!(
            conn.query_row(
                "SELECT active, generation FROM review_lifecycles WHERE id = ?1",
                [lifecycle_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap(),
            (1, 3)
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM review_lifecycle_transitions WHERE lifecycle_id = ?1",
                [lifecycle_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            1
        );
        let indexes = conn
            .prepare("PRAGMA index_list('review_lifecycles')")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            indexes
                .iter()
                .any(|name| name == "review_lifecycles_active_session")
        );
        assert!(
            indexes
                .iter()
                .any(|name| name == "review_lifecycles_active_pr")
        );
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v29_types_legacy_advisories_as_session_coordination() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..28].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO advisories (
                 identity, severity, paths_json, evidence_json, created_at,
                 resolution_state
             ) VALUES ('legacy', 'warning', '[]', '[]', 1, 'outstanding')",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert_eq!(
            conn.query_row(
                "SELECT audience, producer FROM advisories WHERE identity = 'legacy'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap(),
            ("session".into(), "coordination".into())
        );
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    /// The unique index is the whole point of the table: it is what turns
    /// "the scheduler already decided not to ask again" into a fact the
    /// database enforces across a crash. Without it a re-run of the executor
    /// spawns a second reviewer on the same head.
    #[test]
    fn v33_makes_one_review_per_head_a_database_fact() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let now = 1_700_000_000_000_i64;
        let insert = "INSERT INTO review_requests
             (repository, pr_number, review_type, head_commit, backend, state,
              requested_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'requested', ?6, ?6)";
        conn.execute(
            insert,
            rusqlite::params!["o/r", 7, "security", "abc", "chau7", now],
        )
        .unwrap();
        let duplicate = conn.execute(
            insert,
            rusqlite::params!["o/r", 7, "security", "abc", "chau7", now],
        );
        assert!(
            duplicate.is_err(),
            "a second request for the same head must be refused"
        );

        // A new head is a new review, and a different dimension on the same
        // head is too -- the index constrains the pair, not the pull request.
        conn.execute(
            insert,
            rusqlite::params!["o/r", 7, "security", "def", "chau7", now],
        )
        .unwrap();
        conn.execute(
            insert,
            rusqlite::params!["o/r", 7, "performance", "abc", "codex", now],
        )
        .unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    /// v37 widens the state CHECK, which SQLite can only do by rebuilding the
    /// table. A rebuild is where a column added by an `ALTER` quietly goes
    /// missing, and v36's `base_commit` is exactly such a column -- losing it
    /// would silently turn every `head_and_base` dimension back into `head`.
    #[test]
    fn v37_admits_a_waived_review_and_carries_the_recorded_base() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..36].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }
        let now = 1_700_000_000_000_i64;
        let insert = "INSERT INTO review_requests
             (repository, pr_number, review_type, head_commit, base_commit,
              backend, state, detail, requested_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)";
        conn.execute(
            insert,
            rusqlite::params![
                "o/r",
                7,
                "code",
                "abc",
                "base111",
                "chau7",
                "satisfied",
                "shipped",
                now
            ],
        )
        .unwrap();
        // A row with no recorded base, which is every row written before v36.
        conn.execute(
            insert,
            rusqlite::params![
                "o/r",
                7,
                "security",
                "abc",
                None::<String>,
                "chau7",
                "requested",
                None::<String>,
                now
            ],
        )
        .unwrap();

        // `waived` must be impossible before the migration, or this test would
        // pass without v37 doing anything at all.
        assert!(
            conn.execute(
                insert,
                rusqlite::params![
                    "o/r",
                    7,
                    "docs",
                    "abc",
                    None::<String>,
                    "waiver",
                    "waived",
                    "d",
                    now
                ],
            )
            .is_err(),
            "v36 must reject `waived`, otherwise v37 is a no-op"
        );

        migrate(&conn).unwrap();

        let (base, detail): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT base_commit, detail FROM review_requests WHERE review_type = 'code'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            base.as_deref(),
            Some("base111"),
            "the rebuild must carry v36's base_commit or freshness silently regresses"
        );
        assert_eq!(detail.as_deref(), Some("shipped"));
        let missing: Option<String> = conn
            .query_row(
                "SELECT base_commit FROM review_requests WHERE review_type = 'security'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            missing, None,
            "and must not invent one for a row that had none"
        );

        conn.execute(
            insert,
            rusqlite::params![
                "o/r",
                7,
                "docs",
                "abc",
                None::<String>,
                "waiver",
                "waived",
                "d",
                now
            ],
        )
        .expect("v37 admits the waived state");

        assert!(
            conn.execute(
                insert,
                rusqlite::params![
                    "o/r",
                    7,
                    "perf",
                    "abc",
                    None::<String>,
                    "chau7",
                    "pending",
                    None::<String>,
                    now
                ],
            )
            .is_err(),
            "the state CHECK must survive the rebuild"
        );
        assert!(
            conn.execute(
                insert,
                rusqlite::params![
                    "o/r",
                    7,
                    "docs",
                    "abc",
                    None::<String>,
                    "waiver",
                    "waived",
                    "d",
                    now
                ],
            )
            .is_err(),
            "the one-review-per-head index must survive the rebuild"
        );
    }

    /// v38 makes request and completion provenance independent. Existing rows
    /// are request facts from the old schema, so their request head is copied
    /// explicitly while completion facts remain unknown rather than inferred.
    #[test]
    fn v38_adds_typed_completion_facts_and_allows_unsolicited_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..37].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO review_requests
             (repository, pr_number, review_type, head_commit, base_commit,
              backend, state, detail, requested_at, updated_at)
             VALUES ('o/r', 7, 'security', 'requested', NULL, 'chau7',
                     'requested', NULL, 100, 100)",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let columns = conn
            .prepare("PRAGMA table_info(review_requests)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for expected in [
            "requested_for_commit",
            "trigger",
            "completed_at",
            "completed_for_commit",
            "verdict",
            "reviewer_provider",
            "reviewer_model",
        ] {
            assert!(
                columns.iter().any(|column| column == expected),
                "{expected}"
            );
        }
        let old: (Option<String>, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT requested_for_commit, completed_for_commit, verdict
                 FROM review_requests WHERE review_type = 'security'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(old, (Some("requested".into()), None, None));

        conn.execute(
            "INSERT INTO review_requests (
                 repository, pr_number, review_type, head_commit,
                 trigger, backend, state, requested_at, completed_at,
                 completed_for_commit, verdict, reviewer_provider,
                 reviewer_model, updated_at
             ) VALUES ('o/r', 7, 'code', 'completed', 'unsolicited',
                       'unsolicited', 'satisfied', NULL, 200, 'completed',
                       'pass', 'github', 'reviewer-model', 200)",
            [],
        )
        .unwrap();
        let unsolicited: (Option<i64>, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT requested_at, trigger, completed_for_commit
                 FROM review_requests WHERE review_type = 'code'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            unsolicited,
            (None, Some("unsolicited".into()), Some("completed".into()))
        );
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn v39_adds_releasable_checkpoint_pins_and_expired_exposures() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..38].iter().enumerate() {
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
                 id, worktree_path, branch, origin, created_at, updated_at,
                 last_activity_at
             ) VALUES (1, '/tmp/worktree', 'branch', 'adopted', 1, 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO merge_queue (
                 id, session_id, head_commit, base_commit, status, created_at, updated_at
             ) VALUES (1, 1, 'head', 'base', 'promoted', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entry_path_exposures (
                 queue_entry_id, promotion_sha, paths_json, created_at, state
             ) VALUES (1, 'promotion', '[\"src/lib.rs\"]', 1, 'outstanding')",
            [],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let preserved: (String, String) = conn
            .query_row(
                "SELECT promotion_sha, state
                 FROM entry_path_exposures WHERE queue_entry_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(preserved, ("promotion".into(), "outstanding".into()));
        conn.execute(
            "INSERT INTO gc_checkpoint_pin_releases
                 (session_id, queue_entry_id, released_at)
             VALUES (1, 1, 2)",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE entry_path_exposures
             SET state = 'expired', resolved_at = 2,
                 resolution_kind = 'expired'
             WHERE queue_entry_id = 1",
            [],
        )
        .unwrap();
        assert!(
            conn.execute(
                "UPDATE entry_path_exposures
                 SET state = 'unknown' WHERE queue_entry_id = 1",
                [],
            )
            .is_err()
        );
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    /// v33 spelled two different outcomes `abandoned`: a review the policy
    /// deliberately performs nothing for, and a review nobody managed to ask
    /// for. Only the second may be asked for again, so the ledger could not
    /// answer its own question without also reading `backend`. v34 gives the
    /// first its own name and leaves `abandoned` meaning exactly one thing.
    #[test]
    fn v34_separates_a_settled_record_from_a_review_nobody_was_asked_for() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..33].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }
        let now = 1_700_000_000_000_i64;
        let insert = "INSERT INTO review_requests
             (repository, pr_number, review_type, head_commit, backend, state,
              requested_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)";
        // The two v33 rows that need telling apart, plus one that must not move.
        conn.execute(
            insert,
            rusqlite::params!["o/r", 7, "docs", "abc", "record", "abandoned", now],
        )
        .unwrap();
        conn.execute(
            insert,
            rusqlite::params!["o/r", 7, "security", "abc", "chau7", "abandoned", now],
        )
        .unwrap();
        conn.execute(
            insert,
            rusqlite::params!["o/r", 7, "code", "abc", "chau7", "satisfied", now],
        )
        .unwrap();

        migrate(&conn).unwrap();

        let state = |review_type: &str| -> String {
            conn.query_row(
                "SELECT state FROM review_requests WHERE review_type = ?1",
                [review_type],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            state("docs"),
            "recorded",
            "a record-only row was always settled, not waiting to be retried"
        );
        assert_eq!(
            state("security"),
            "abandoned",
            "a routed review nobody was asked for stays the one revivable state"
        );
        assert_eq!(state("code"), "satisfied");

        // The identity that makes "already requested for this head" durable
        // has to survive the table rebuild, or v34 quietly undoes v33.
        let duplicate = conn.execute(
            insert,
            rusqlite::params!["o/r", 7, "code", "abc", "chau7", "requested", now],
        );
        assert!(
            duplicate.is_err(),
            "the one-review-per-head index must survive the rebuild"
        );
        assert!(
            conn.execute(
                insert,
                rusqlite::params!["o/r", 7, "code", "abc", "chau7", "pending", now],
            )
            .is_err(),
            "the state CHECK must survive the rebuild"
        );
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    /// The router used to report `pull_request_opened` for every tick, because
    /// it had nothing to compare against. v35 is that missing half: one row per
    /// pull request holding what the last tick saw, so the next one can name
    /// the transition instead of assuming it.
    #[test]
    fn v35_remembers_exactly_one_previous_observation_per_pull_request() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .unwrap();
        for (index, sql) in MIGRATIONS[..34].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [(index + 1).to_string()],
            )
            .unwrap();
        }

        migrate(&conn).unwrap();

        let insert = "INSERT INTO pull_request_observations
             (repository, pr_number, head_commit, base_ref, is_draft, state,
              dismissed_reviews, observed_at)
             VALUES (?1, ?2, ?3, 'main', 0, 'open', 0, 1)";
        conn.execute(insert, rusqlite::params!["o/r", 7, "abc"])
            .unwrap();
        // Two observations of one pull request would make "what did we see
        // last time" ambiguous, and the derivation has no tiebreaker: it reads
        // one row or none. The key is what keeps the write an upsert.
        assert!(
            conn.execute(insert, rusqlite::params!["o/r", 7, "def"])
                .is_err(),
            "a pull request must have at most one remembered observation"
        );
        // Another pull request, and the same number in another repository, are
        // both different subjects.
        conn.execute(insert, rusqlite::params!["o/r", 8, "abc"])
            .unwrap();
        conn.execute(insert, rusqlite::params!["o/other", 7, "abc"])
            .unwrap();
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    /// The database this machine had on 2026-10-07: a build from the closed
    /// #564 stamped "v48" after adding `agent_provenance_json`, so #611's v48
    /// tables were never created and #613's v49 failed on the duplicate column.
    fn database_with_a_foreign_v48() -> Connection {
        let conn = migrated_through(47);
        conn.execute_batch(
            "ALTER TABLE coordinated_operations ADD COLUMN agent_provenance_json TEXT",
        )
        .unwrap();
        set_meta(&conn, "schema_version", 48);
        conn
    }

    fn repository_watch_tables_present(conn: &Connection) -> bool {
        TABLE_MIGRATIONS
            .iter()
            .flat_map(|(_, _, tables)| tables.iter())
            .all(|table| table_exists(conn, table).unwrap())
    }

    #[test]
    fn a_database_stamped_by_a_foreign_v48_opens_and_gains_the_real_v48_tables() {
        let conn = database_with_a_foreign_v48();
        assert!(!repository_watch_tables_present(&conn));

        migrate(&conn).unwrap();

        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
        assert!(repository_watch_tables_present(&conn));
        assert!(column_exists(&conn, "coordinated_operations", "agent_provenance_json").unwrap());
        assert!(table_exists(&conn, "repository_watches").unwrap());
    }

    #[test]
    fn a_current_database_missing_recorded_objects_is_repaired_on_open() {
        let conn = migrated_through(47);
        conn.execute_batch(
            "ALTER TABLE coordinated_operations ADD COLUMN agent_provenance_json TEXT",
        )
        .unwrap();
        set_meta(&conn, "schema_version", SCHEMA_VERSION);

        migrate(&conn).unwrap();

        assert!(repository_watch_tables_present(&conn));
    }

    #[test]
    fn migrating_twice_is_a_no_op() {
        let conn = migrated_through(47);
        migrate(&conn).unwrap();
        let objects = |conn: &Connection| -> Vec<String> {
            let mut stmt = conn
                .prepare("SELECT type || ':' || name FROM sqlite_master ORDER BY 1")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let before = objects(&conn);
        migrate(&conn).unwrap();
        assert_eq!(objects(&conn), before);
        assert_eq!(current_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn idempotent_creates_rewrites_every_create_once() {
        let sql =
            "CREATE TABLE a (x);\nCREATE INDEX i ON a (x);\nCREATE TABLE IF NOT EXISTS b (y);";
        let out = idempotent_creates(sql);
        assert_eq!(out.matches("IF NOT EXISTS").count(), 3);
        assert!(!out.contains("IF NOT EXISTS IF NOT EXISTS"));
    }
}
