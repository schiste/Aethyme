-- Frozen schema migration source: v0.8.28 commit ece4094837080a11ecae995db03178234834f876
-- schema.rs blob 418cac7cceacc5420fbb83abd41c80a359f9dc25; source SHA-256 39f09db29e4c761d822ab0ee2ef6efb3d02e0361808b8ef3156586334a880815
-- Applies each released migration in its own transaction and stamps its version.
PRAGMA foreign_keys = ON;
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
BEGIN IMMEDIATE;
INSERT INTO meta (key, value) VALUES ('schema_version', '0');
COMMIT;
-- v1
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '1') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v2
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '2') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v3
BEGIN IMMEDIATE;

CREATE TABLE session_foreign_files (
    id          INTEGER PRIMARY KEY,
    session_id  INTEGER NOT NULL REFERENCES sessions (id),
    path        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    UNIQUE (session_id, path)
);

CREATE INDEX session_foreign_files_by_session
    ON session_foreign_files (session_id, path);
INSERT INTO meta (key, value) VALUES ('schema_version', '3') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v4
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '4') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v5
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '5') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v6
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '6') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v7
BEGIN IMMEDIATE;

ALTER TABLE integration_reconciliation_intent
    ADD COLUMN plan_digest TEXT NOT NULL DEFAULT '';
ALTER TABLE integration_reconciliations
    ADD COLUMN plan_digest TEXT NOT NULL DEFAULT '';
INSERT INTO meta (key, value) VALUES ('schema_version', '7') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v8
BEGIN IMMEDIATE;

ALTER TABLE gates ADD COLUMN resources_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE gates ADD COLUMN resource_ttl_seconds INTEGER NOT NULL DEFAULT 300;
ALTER TABLE gates ADD COLUMN definition_hash TEXT NOT NULL DEFAULT '';
ALTER TABLE gate_results ADD COLUMN definition_hash TEXT NOT NULL DEFAULT '';
CREATE INDEX gate_results_by_gate_tree_definition
    ON gate_results (gate_name, tree_hash, definition_hash, id);
INSERT INTO meta (key, value) VALUES ('schema_version', '8') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v9
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '9') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v10
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '10') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v11
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '11') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v12
BEGIN IMMEDIATE;

-- Gate resource contention is expected when independent clones share a host.
-- Persist the bounded wait policy so historical definitions remain auditable.
ALTER TABLE gates ADD COLUMN resource_wait_seconds INTEGER NOT NULL DEFAULT 0;
INSERT INTO meta (key, value) VALUES ('schema_version', '12') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v13
BEGIN IMMEDIATE;

-- Broker-owned artifact cache policy is stored separately from generic host
-- resource declarations so old gate results remain explainable.
ALTER TABLE gates ADD COLUMN managed_cache_json TEXT;
INSERT INTO meta (key, value) VALUES ('schema_version', '13') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v14
BEGIN IMMEDIATE;

-- Content-free execution telemetry separates coordination delay, command
-- startup, and logging volume without storing command output.
ALTER TABLE gate_results ADD COLUMN wait_duration_ms INTEGER;
ALTER TABLE gate_results ADD COLUMN first_output_ms INTEGER;
ALTER TABLE gate_results ADD COLUMN output_bytes INTEGER;
INSERT INTO meta (key, value) VALUES ('schema_version', '14') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v15
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '15') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v16
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '16') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v17
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '17') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v18
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '18') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v19
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '19') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v20
BEGIN IMMEDIATE;

-- Retention planning walks age cutoffs and terminal status without scanning
-- the append-only tables from the beginning on every bounded maintenance run.
CREATE INDEX events_by_retention_age ON events (ts, id);
CREATE INDEX gate_results_by_retention_age ON gate_results (created_at, id);
CREATE INDEX merge_queue_by_retention_status_age
    ON merge_queue (status, updated_at, id);
CREATE INDEX sessions_by_cleanup_age
    ON sessions (cleanup_state, closed_at, id);
INSERT INTO meta (key, value) VALUES ('schema_version', '20') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v21
BEGIN IMMEDIATE;

-- Default status reads only the latest row for each live session; terminal
-- history is served separately through a newest-first id cursor.
CREATE INDEX merge_queue_by_session_id
    ON merge_queue (session_id, id DESC);
INSERT INTO meta (key, value) VALUES ('schema_version', '21') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v22
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '22') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v23
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '23') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v24
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '24') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v25
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '25') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v26
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '26') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v27
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '27') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v28
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '28') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v29
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '29') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v30
BEGIN IMMEDIATE;

-- Suppression is a maintainer-only control layered over the established
-- acknowledged state, so widening the durable state constraint is unnecessary.
ALTER TABLE advisories ADD COLUMN suppressed_at INTEGER;
INSERT INTO meta (key, value) VALUES ('schema_version', '30') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v31
BEGIN IMMEDIATE;

ALTER TABLE sessions ADD COLUMN agent_identity TEXT;
INSERT INTO meta (key, value) VALUES ('schema_version', '31') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v32
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '32') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v33
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '33') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v34
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '34') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v35
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '35') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v36
BEGIN IMMEDIATE;

ALTER TABLE review_requests ADD COLUMN base_commit TEXT;
ALTER TABLE pull_request_observations ADD COLUMN base_commit TEXT;
INSERT INTO meta (key, value) VALUES ('schema_version', '36') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v37
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '37') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v38
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '38') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v39
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '39') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v40
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '40') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v41
BEGIN IMMEDIATE;

ALTER TABLE sessions ADD COLUMN repository_name TEXT;
ALTER TABLE sessions ADD COLUMN tab_name TEXT;
ALTER TABLE sessions ADD COLUMN ai_provider TEXT;
INSERT INTO meta (key, value) VALUES ('schema_version', '41') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v42
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '42') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v43
BEGIN IMMEDIATE;

ALTER TABLE gate_results ADD COLUMN load_avg_1m_start REAL;
ALTER TABLE gate_results ADD COLUMN load_avg_1m_end REAL;
ALTER TABLE gate_results ADD COLUMN cpu_count INTEGER;
ALTER TABLE gate_results ADD COLUMN free_disk_bytes_start INTEGER;
INSERT INTO meta (key, value) VALUES ('schema_version', '43') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v44
BEGIN IMMEDIATE;

ALTER TABLE sessions ADD COLUMN short_name TEXT;
INSERT INTO meta (key, value) VALUES ('schema_version', '44') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v45
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '45') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v46
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '46') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v47
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '47') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
INSERT INTO meta (key, value) VALUES ('min_compatible_schema', '47') ON CONFLICT (key) DO UPDATE SET value = excluded.value WHERE CAST(meta.value AS INTEGER) < CAST(excluded.value AS INTEGER);
COMMIT;
-- v48
BEGIN IMMEDIATE;

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
INSERT INTO meta (key, value) VALUES ('schema_version', '48') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v49
BEGIN IMMEDIATE;

-- Keep coordinated-operation caller/holder provenance local and nullable so
-- older compatible writers may continue to create history rows.
ALTER TABLE coordinated_operations ADD COLUMN agent_provenance_json TEXT;
INSERT INTO meta (key, value) VALUES ('schema_version', '49') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v50
BEGIN IMMEDIATE;

-- Keep the durable candidate identity typed and separate from details_json,
-- which remains a compatibility projection for older broker readers.
CREATE TABLE verification_candidates (
    queue_entry_id INTEGER PRIMARY KEY REFERENCES merge_queue (id) ON DELETE CASCADE,
    base_commit   TEXT NOT NULL,
    inputs_json   TEXT NOT NULL,
    tree_id       TEXT NOT NULL,
    commit_id     TEXT NOT NULL
);

CREATE INDEX verification_candidates_by_commit
    ON verification_candidates (commit_id, queue_entry_id);
INSERT INTO meta (key, value) VALUES ('schema_version', '50') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v51
BEGIN IMMEDIATE;

-- No schema change: this version marks binaries that implement required
-- collaboration capture, so a repository can fence older ones out.
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT INTO meta (key, value) VALUES ('schema_version', '51') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
COMMIT;
-- v52
BEGIN IMMEDIATE;

ALTER TABLE gate_results ADD COLUMN execution_profile_hash TEXT;
INSERT INTO meta (key, value) VALUES ('schema_version', '52') ON CONFLICT (key) DO UPDATE SET value = excluded.value;
INSERT INTO meta (key, value) VALUES ('min_compatible_schema', '52') ON CONFLICT (key) DO UPDATE SET value = excluded.value WHERE CAST(meta.value AS INTEGER) < CAST(excluded.value AS INTEGER);
COMMIT;
