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
pub const SCHEMA_VERSION: i64 = 28;

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
}
