//! Retained collaboration state, kept apart from broker state and from every
//! legacy cleanup root (#656, plan §6.3, §10.1; D09, D18, D20, D25).
//!
//! ```text
//! <host state>/collaboration/
//!   .aethyme-collaboration-root.json
//!   <project key>/
//!     state.db          one database per project
//!     objects/, spool/  added by the archive and capture slices (#657, #658)
//! ```
//!
//! - **Location.** The root is derived from the host state directory only,
//!   never from a worktree container: no existing deleter enumerates the host
//!   state directory itself (L0 slice C §2-3), so a binary that predates this
//!   module cannot reach it. Opening refuses a root that equals, contains or
//!   lies inside a worktree container, the host cache or the repository
//!   checkout, and a root inside any Git working tree. A refusal names the
//!   setting to change; nothing is moved or repaired implicitly.
//! - **Ownership.** Directories are created `0700` and must stay owned by
//!   the caller with no group or other access; looser ones are refused, not
//!   repaired. A non-empty root without the root marker is refused as foreign.
//! - **Durability.** `state.db` runs in WAL mode with `synchronous=FULL` and
//!   `fullfsync`, so a returned commit has been flushed to the device where
//!   the platform offers that. The filesystem decides whether this is the
//!   supported profile; an unsupported one still opens, and says so in
//!   [`DurabilityProfile`] so a receipt can be labelled differently.
//! - **No cross-store transaction.** Nothing here attaches `broker.db` or
//!   any other database. A collaboration record may name a broker session
//!   as a plain value; it never relies on a write to `broker.db` landing
//!   with its own.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;

pub use crate::host_state::HostStateSource;

/// The schema this binary writes.
pub const COLLABORATION_STATE_SCHEMA_VERSION: i64 = 29;
/// The oldest schema a database written by this binary can be read by.
/// Unlike `host-operations.db`, a newer database stays readable by an older
/// binary until a release raises this floor.
///
/// Raised to 4 with reclamation (#659): a schema 3 binary would capture
/// without the archive lock, the reuse refresh or the reclaimed-marker
/// clearing, so reclamation could remove source it had just named. Raised
/// to 5 with contribution context (#661): a schema 4 binary's reclamation
/// does not treat attached brief objects as roots, so it could remove the
/// brief of a live contribution. Raised to 6 with the context cache (#662):
/// a schema 5 binary indexes contributions without broad-risk postings and
/// purges no cached answer when a contribution stops being live, so a newer
/// binary could serve a cached answer a broad change should have
/// invalidated. No release shipped schemas 2 to 5, so nothing that exists
/// is locked out.
// Schema 6 reclamation cannot see local target roots and would delete live
// controller inputs. Such a binary must refuse this store before writing.
// Schema 8 has no budget ledger, and cannot guard retention while consumption is
// unknown. As with prior floors, stop old open clients before migration.
// Schema 9 does not honor external authority epochs. Quiesce it before restore.
// Schema 10 cannot enforce the configured verification producer catalogue.
// Schema 11 cannot fence unresolved source publication against retention release.
// Schema 17 reclamation cannot see independently retained native gate jobs.
// Schema 21 cannot interpret finite controller service appointments.
// Schema 22 cannot interpret explicit native-job root release.
// Schema 23 does not honor the permanent sensitive-response tombstone and
// could read, recapture, or export data after administrative removal.
// Schema 24 does not consume retained-capture notices at the appointed owner
// tick and could run a target while an accepted notice remains queued.
// Schema 26 introduces project/file mappings and immutable contribution
// revision heads. A schema-25 writer would replace a brief without advancing
// that head or rooting superseded brief objects, so stop it before writes.
// Schema 27 adds single-use, session-bound resolution imports and durable
// request identity. Schema 26 writers do not preserve that state or its roots.
// Schema 28 adds an immutable optional resolution-result link; schema 27
// writers can advance a linked wave without preserving that provenance.
// Schema 29 adds immutable Local resolution reservation proofs and their
// one-to-one shared-attempt bindings. Older writers cannot preserve managed
// resolution authority or its idempotency roots.
const MIN_COMPATIBLE_SCHEMA: i64 = 29;
const ROOT_DIRECTORY: &str = "collaboration";
/// Schema steps after the version 1 layout (`meta` only), applied in
/// order. Step 17 adds separately indexed historical imports and rehearsals. The floor is
/// raised to [`MIN_COMPATIBLE_SCHEMA`].
const MIGRATIONS: &[(i64, &str)] = &[
    (
        2,
        // The archive index (#657). Rows are written only after every object a
        // snapshot or contribution names has been published and verified; the
        // objects themselves are the authority, and these rows are rebuildable.
        "CREATE TABLE IF NOT EXISTS retained_snapshots (
         snapshot_id TEXT PRIMARY KEY NOT NULL,
         record_id TEXT NOT NULL,
         record_sha256 TEXT NOT NULL,
         commit_oid TEXT NOT NULL,
         entry_count INTEGER NOT NULL,
         content_bytes INTEGER NOT NULL
     ) STRICT;
     CREATE TABLE IF NOT EXISTS retained_contributions (
         lineage_record_id TEXT PRIMARY KEY NOT NULL,
         record_sha256 TEXT NOT NULL,
         base_snapshot TEXT NOT NULL REFERENCES retained_snapshots (snapshot_id),
         result_snapshot TEXT NOT NULL REFERENCES retained_snapshots (snapshot_id)
     ) STRICT;",
    ),
    (
        3,
        // The capture journal (#658). An operation row is the retry key and
        // the recovery owner; a receipt, its retention root and its outbox
        // row are committed in one transaction. Retention roots, active
        // operations and reservations are what reclamation (#659) consults.
        "CREATE TABLE IF NOT EXISTS capture_operations (
             operation_id TEXT PRIMARY KEY NOT NULL,
             request_digest TEXT NOT NULL,
             repository TEXT NOT NULL,
             base_commit TEXT NOT NULL,
             result_commit TEXT NOT NULL,
             policy TEXT NOT NULL,
             retention_until_ms INTEGER,
             reserved_bytes INTEGER NOT NULL,
             state TEXT NOT NULL CHECK (state IN ('intent', 'copying', 'sealed', 'committed',
                 'acknowledged', 'incomplete', 'failed', 'refused', 'aborted')),
             lineage_record_id TEXT,
             outcome_code TEXT,
             outcome_detail TEXT,
             created_ms INTEGER NOT NULL,
             updated_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS capture_receipts (
             operation_id TEXT PRIMARY KEY NOT NULL
                 REFERENCES capture_operations (operation_id),
             receipt_record_id TEXT NOT NULL,
             receipt_sha256 TEXT NOT NULL,
             lineage_record_id TEXT NOT NULL
                 REFERENCES retained_contributions (lineage_record_id),
             base_snapshot TEXT NOT NULL,
             result_snapshot TEXT NOT NULL,
             durability TEXT NOT NULL,
             committed_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS retention_roots (
             root_id INTEGER PRIMARY KEY,
             kind TEXT NOT NULL CHECK (kind IN ('capture_intent', 'contribution')),
             operation_id TEXT NOT NULL REFERENCES capture_operations (operation_id),
             lineage_record_id TEXT,
             until_ms INTEGER,
             created_ms INTEGER NOT NULL,
             released_ms INTEGER
         ) STRICT;
         CREATE INDEX IF NOT EXISTS retention_roots_live
             ON retention_roots (kind) WHERE released_ms IS NULL;
         CREATE TABLE IF NOT EXISTS outbox (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             operation_id TEXT NOT NULL UNIQUE REFERENCES capture_operations (operation_id),
             kind TEXT NOT NULL,
             payload_record_id TEXT NOT NULL,
             created_ms INTEGER NOT NULL,
             delivered_ms INTEGER
         ) STRICT;",
    ),
    (
        4,
        // Reclamation (#659). Reader leases and object pins are roots beside
        // retention roots. A reclaimed snapshot or contribution keeps its
        // index row (receipts name it) and gets a marker instead. A
        // generation journals one apply so an interrupted one resumes.
        "CREATE TABLE IF NOT EXISTS reader_leases (
             lease_id INTEGER PRIMARY KEY,
             target_kind TEXT NOT NULL CHECK (target_kind IN ('snapshot', 'contribution')),
             target TEXT NOT NULL,
             holder TEXT NOT NULL,
             until_ms INTEGER NOT NULL,
             created_ms INTEGER NOT NULL,
             released_ms INTEGER
         ) STRICT;
         CREATE TABLE IF NOT EXISTS object_pins (
             pin_id INTEGER PRIMARY KEY,
             class TEXT NOT NULL CHECK (class IN ('analysis_view', 'cited_evidence')),
             object_sha256 TEXT NOT NULL,
             holder TEXT NOT NULL,
             until_ms INTEGER,
             created_ms INTEGER NOT NULL,
             released_ms INTEGER
         ) STRICT;
         CREATE TABLE IF NOT EXISTS reclaimed_snapshots (
             snapshot_id TEXT PRIMARY KEY NOT NULL,
             generation INTEGER NOT NULL,
             reclaimed_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS reclaimed_contributions (
             lineage_record_id TEXT PRIMARY KEY NOT NULL,
             generation INTEGER NOT NULL,
             reclaimed_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS gc_plans (
             digest TEXT PRIMARY KEY NOT NULL,
             body TEXT NOT NULL,
             created_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS gc_generations (
             generation INTEGER PRIMARY KEY,
             plan_digest TEXT NOT NULL,
             state TEXT NOT NULL CHECK (state IN ('trashing', 'trashed', 'done')),
             started_ms INTEGER NOT NULL,
             finished_ms INTEGER
         ) STRICT;
         CREATE TABLE IF NOT EXISTS gc_trash (
             generation INTEGER NOT NULL REFERENCES gc_generations (generation),
             relpath TEXT NOT NULL,
             bytes INTEGER NOT NULL,
             PRIMARY KEY (generation, relpath)
         ) STRICT;",
    ),
    (
        5,
        // Contribution context (#661). `contribution_briefs` is authority:
        // which brief explains a contribution (a revision replaces it).
        // `context_postings`, `context_indexed` and `context_unreadable`
        // are derived from the archive and rebuildable at any time.
        "CREATE TABLE IF NOT EXISTS contribution_briefs (
             lineage_record_id TEXT PRIMARY KEY NOT NULL
                 REFERENCES retained_contributions (lineage_record_id),
             brief_record_id TEXT NOT NULL,
             brief_sha256 TEXT NOT NULL,
             attached_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS context_postings (
             key BLOB NOT NULL,
             lineage_record_id TEXT NOT NULL,
             PRIMARY KEY (key, lineage_record_id)
         ) STRICT;
         CREATE INDEX IF NOT EXISTS context_postings_by_contribution
             ON context_postings (lineage_record_id);
         CREATE TABLE IF NOT EXISTS context_indexed (
             lineage_record_id TEXT PRIMARY KEY NOT NULL,
             brief_record_id TEXT
         ) STRICT;
         CREATE TABLE IF NOT EXISTS context_unreadable (
             lineage_record_id TEXT PRIMARY KEY NOT NULL,
             generation INTEGER NOT NULL
         ) STRICT;",
    ),
    (
        6,
        // The context cache (#662). Every table here is derived: answers are
        // stored under their cache key, which commits to every input that can
        // change them. The derived index is rebuilt on first use, because it
        // carries no `context_index_format` stamp yet (collaboration_context).
        "CREATE TABLE IF NOT EXISTS context_cache (
             cache_key TEXT PRIMARY KEY NOT NULL,
             visibility TEXT NOT NULL,
             epoch INTEGER NOT NULL,
             record BLOB NOT NULL,
             record_id TEXT NOT NULL,
             contributions TEXT NOT NULL,
             refetch_after_ms INTEGER,
             computed_ms INTEGER NOT NULL,
             generation INTEGER NOT NULL,
             last_used_ms INTEGER NOT NULL
         ) STRICT;
         CREATE TABLE IF NOT EXISTS context_cache_members (
             cache_key TEXT NOT NULL REFERENCES context_cache (cache_key) ON DELETE CASCADE,
             lineage_record_id TEXT NOT NULL,
             PRIMARY KEY (cache_key, lineage_record_id)
         ) STRICT;
         CREATE INDEX IF NOT EXISTS context_cache_members_by_contribution
             ON context_cache_members (lineage_record_id);",
    ),
    (
        7,
        // L5 targets and their permanent roots. Adopt #665's existing lazy
        // attempt ledger without replacing rows or replenishing allowances.
        "CREATE TABLE IF NOT EXISTS local_targets (
             target TEXT PRIMARY KEY NOT NULL,
             issuer TEXT NOT NULL,
             active_revision TEXT,
             phase TEXT NOT NULL DEFAULT 'disabled',
             resume_phase TEXT,
             version INTEGER NOT NULL DEFAULT 0
         ) STRICT;
         CREATE TABLE IF NOT EXISTS local_target_revisions (
             revision TEXT PRIMARY KEY NOT NULL,
             target TEXT NOT NULL REFERENCES local_targets (target),
             body BLOB NOT NULL,
             released INTEGER NOT NULL DEFAULT 0 CHECK (released IN (0, 1))
         ) STRICT;
         CREATE TABLE IF NOT EXISTS local_target_dispositions (
             revision TEXT NOT NULL REFERENCES local_target_revisions (revision),
             contribution TEXT NOT NULL,
             status TEXT NOT NULL,
             detail TEXT,
             PRIMARY KEY (revision, contribution)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS local_target_roots (
             revision TEXT NOT NULL REFERENCES local_target_revisions (revision),
             kind TEXT NOT NULL CHECK (kind IN ('snapshot', 'contribution', 'object')),
             reference TEXT NOT NULL,
             PRIMARY KEY (revision, kind, reference)
         ) STRICT;
         CREATE TABLE IF NOT EXISTS resolution_attempts (
             attempt_id INTEGER PRIMARY KEY,
             decision_key TEXT NOT NULL,
             kind TEXT NOT NULL,
             members TEXT NOT NULL,
             paths TEXT NOT NULL,
             request_record_id TEXT NOT NULL,
             resolver TEXT NOT NULL,
             outcome TEXT,
             charged_ms INTEGER NOT NULL
         ) STRICT;",
    ),
    (
        8,
        "CREATE TABLE local_wave_arrivals (
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,
             revision TEXT NOT NULL REFERENCES local_target_revisions (revision),
             contribution TEXT NOT NULL,
             first_seen_ms INTEGER NOT NULL CHECK (first_seen_ms >= 0),
             UNIQUE (revision, contribution)
         ) STRICT;
         CREATE TABLE local_waves (
             wave TEXT PRIMARY KEY NOT NULL,
             target TEXT NOT NULL REFERENCES local_targets (target),
             revision TEXT NOT NULL REFERENCES local_target_revisions (revision),
             sequence INTEGER NOT NULL,
             body BLOB NOT NULL,
             phase TEXT NOT NULL,
             candidate_snapshot TEXT,
             candidate_commit TEXT,
             recipe_object TEXT,
             detail TEXT,
             composition_ms INTEGER NOT NULL DEFAULT 0,
             drift_count INTEGER NOT NULL DEFAULT 0,
             last_observed_commit TEXT,
             UNIQUE (target, sequence)
         ) STRICT;
         CREATE UNIQUE INDEX local_waves_one_active ON local_waves (target)
             WHERE phase IN ('frozen', 'composing', 'verifying', 'outcome_unknown');",
    ),
    (
        9,
        r#"
        CREATE TABLE IF NOT EXISTS local_budget_project (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1), issuer TEXT NOT NULL,
            policy TEXT NOT NULL, body BLOB NOT NULL
        );
        CREATE TABLE IF NOT EXISTS local_budget_objectives (
            objective TEXT PRIMARY KEY, issuer TEXT NOT NULL, policy TEXT NOT NULL, body BLOB NOT NULL
        );
        CREATE TABLE IF NOT EXISTS local_budget_reservations (
            reservation TEXT PRIMARY KEY, nonce TEXT NOT NULL UNIQUE,
            target TEXT NOT NULL REFERENCES local_targets(target),
            revision TEXT NOT NULL REFERENCES local_target_revisions(revision),
            wave TEXT NOT NULL REFERENCES local_waves(wave),
            root TEXT NOT NULL, parent TEXT, body BLOB NOT NULL,
            phase TEXT NOT NULL CHECK(phase IN ('reserved','partly_consumed','completion_pending','consumed','released_unused','consumption_unknown')),
            spent BLOB NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
            version INTEGER NOT NULL DEFAULT 0, receipt TEXT, detail TEXT, observed_overrun BLOB
        );
        CREATE TABLE IF NOT EXISTS local_budget_receipts (
            receipt TEXT PRIMARY KEY NOT NULL,
            reservation TEXT NOT NULL REFERENCES local_budget_reservations(reservation),
            attempt INTEGER NOT NULL, usage BLOB NOT NULL
        ) STRICT;
        CREATE INDEX IF NOT EXISTS local_budget_roots
 ON local_budget_reservations(root,parent);
    "#,
    ),
    (
        10,
        "CREATE TABLE local_authority_state (singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton=1), epoch TEXT NOT NULL) STRICT;",
    ),
    (
        11,
        "CREATE TABLE local_verifier_producers (
            target TEXT NOT NULL REFERENCES local_targets(target),
            producer TEXT NOT NULL,
            generation INTEGER NOT NULL CHECK(generation>0),
            policy TEXT NOT NULL,
            body BLOB NOT NULL,
            epoch TEXT NOT NULL,
            revoked INTEGER NOT NULL CHECK(revoked IN (0,1)),
            PRIMARY KEY(target,producer)
        ) STRICT;
        CREATE TABLE local_verification_evidence (
            id TEXT PRIMARY KEY NOT NULL,
            object TEXT NOT NULL,
            revision TEXT NOT NULL REFERENCES local_target_revisions(revision),
            producer TEXT NOT NULL,
            generation INTEGER NOT NULL,
            issued_ms INTEGER NOT NULL,
            expires_ms INTEGER NOT NULL,
            body BLOB NOT NULL
        ) STRICT;",
    ),
    (
        12,
        "CREATE TABLE local_source_transports (
            intent TEXT PRIMARY KEY NOT NULL,
            revision TEXT NOT NULL REFERENCES local_target_revisions(revision),
            target TEXT NOT NULL REFERENCES local_targets(target),
            peer TEXT NOT NULL, destination TEXT NOT NULL,
            body BLOB NOT NULL, dispatched INTEGER NOT NULL CHECK(dispatched IN (0,1)),
            observation TEXT NOT NULL CHECK(observation IN ('unobserved','available','absent','other','unavailable')),
            receipt BLOB, operation INTEGER, write_outcome TEXT
        ) STRICT;
        CREATE TABLE local_source_transport_receipts (
            receipt TEXT PRIMARY KEY NOT NULL,
            intent TEXT NOT NULL REFERENCES local_source_transports(intent),
            body BLOB NOT NULL
        ) STRICT;",
    ),
    (13, "CREATE TABLE local_acceptance_bindings (
        id TEXT PRIMARY KEY NOT NULL, revision TEXT NOT NULL REFERENCES local_target_revisions(revision),
        body BLOB NOT NULL, last_receipt TEXT
    ) STRICT;
    CREATE TABLE local_acceptance_receipts (
        id TEXT PRIMARY KEY NOT NULL, binding TEXT NOT NULL REFERENCES local_acceptance_bindings(id),
        body BLOB NOT NULL
    ) STRICT;
    CREATE TABLE local_acceptance_qualifications (
        id TEXT PRIMARY KEY NOT NULL, binding TEXT NOT NULL REFERENCES local_acceptance_bindings(id),
        body BLOB NOT NULL
    ) STRICT;"),
    (14, "CREATE TABLE local_generations (
        id TEXT PRIMARY KEY NOT NULL, target TEXT NOT NULL REFERENCES local_targets(target),
        operation TEXT NOT NULL, revision TEXT NOT NULL REFERENCES local_target_revisions(revision),
        epoch TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('intent','prepared','adopted','retired')),
        body BLOB NOT NULL, prepared_receipt TEXT, UNIQUE(target,operation)
    ) STRICT;
    CREATE TABLE local_generation_receipts (
        id TEXT PRIMARY KEY NOT NULL, generation TEXT NOT NULL REFERENCES local_generations(id), body BLOB NOT NULL
    ) STRICT;
    CREATE TABLE local_generation_roots (
        generation TEXT NOT NULL REFERENCES local_generations(id),
        kind TEXT NOT NULL CHECK(kind IN ('snapshot','contribution','object')), reference TEXT NOT NULL,
        PRIMARY KEY(generation,kind,reference)
    ) STRICT;"),
    (15, "DROP TRIGGER IF EXISTS outbox_capture_insert; DROP TRIGGER IF EXISTS outbox_capture_update; DROP TRIGGER IF EXISTS outbox_capture_delete; CREATE TABLE local_exchange_bindings (\n id TEXT PRIMARY KEY NOT NULL, epoch TEXT NOT NULL, body BLOB NOT NULL,\n enabled INTEGER NOT NULL CHECK(enabled IN (0,1))\n) STRICT;\nALTER TABLE outbox RENAME TO outbox_capture_v3;\nCREATE TABLE outbox (\n sequence INTEGER PRIMARY KEY AUTOINCREMENT, operation_id TEXT NOT NULL UNIQUE,\n kind TEXT NOT NULL, payload_record_id TEXT NOT NULL, created_ms INTEGER NOT NULL,\n delivered_ms INTEGER\n) STRICT;\nINSERT INTO outbox SELECT * FROM outbox_capture_v3;\nDROP TABLE outbox_capture_v3;\nCREATE TRIGGER outbox_capture_insert BEFORE INSERT ON outbox\n WHEN NEW.kind != 'exchange_metadata_v0' AND NOT EXISTS\n (SELECT 1 FROM capture_operations WHERE operation_id=NEW.operation_id)\n BEGIN SELECT RAISE(ABORT,'capture outbox operation is missing'); END;\nCREATE TRIGGER outbox_capture_update BEFORE UPDATE OF operation_id,kind ON outbox\n WHEN NEW.kind != 'exchange_metadata_v0' AND NOT EXISTS\n (SELECT 1 FROM capture_operations WHERE operation_id=NEW.operation_id)\n BEGIN SELECT RAISE(ABORT,'capture outbox operation is missing'); END;\nCREATE TRIGGER outbox_capture_delete BEFORE DELETE ON capture_operations\n WHEN EXISTS (SELECT 1 FROM outbox WHERE operation_id=OLD.operation_id AND kind != 'exchange_metadata_v0')\n BEGIN SELECT RAISE(ABORT,'capture operation has an outbox message'); END;\nCREATE TABLE local_exchange_deliveries (\n binding TEXT NOT NULL REFERENCES local_exchange_bindings(id),\n sequence INTEGER NOT NULL REFERENCES outbox(sequence), attempts INTEGER NOT NULL CHECK(attempts>=0),\n acknowledged INTEGER NOT NULL CHECK(acknowledged IN (0,1)), ack BLOB,\n PRIMARY KEY(binding,sequence)\n) STRICT;\nCREATE TABLE local_exchange_streams (\n binding TEXT PRIMARY KEY NOT NULL REFERENCES local_exchange_bindings(id),\n witness BLOB NOT NULL, position INTEGER NOT NULL, snapshot_watermark INTEGER NOT NULL,\n valid INTEGER NOT NULL CHECK(valid IN (0,1))\n) STRICT;\nCREATE TABLE local_exchange_events (\n binding TEXT NOT NULL REFERENCES local_exchange_bindings(id), stream TEXT NOT NULL,\n sequence INTEGER NOT NULL, id TEXT NOT NULL, body BLOB NOT NULL, applied INTEGER NOT NULL CHECK(applied IN (0,1,2)),\n PRIMARY KEY(binding,stream,sequence)\n) STRICT;\nCREATE TABLE local_exchange_projection (\n binding TEXT NOT NULL REFERENCES local_exchange_bindings(id),\n packet TEXT NOT NULL, body BLOB NOT NULL, PRIMARY KEY(binding,packet)\n) STRICT;\nCREATE TABLE local_exchange_observations (\n binding TEXT NOT NULL REFERENCES local_exchange_bindings(id), operation TEXT NOT NULL,\n attempts INTEGER NOT NULL CHECK(attempts>=0), PRIMARY KEY(binding,operation)\n) STRICT;\n\nCREATE TABLE local_exchange_messages(sequence INTEGER PRIMARY KEY NOT NULL REFERENCES outbox(sequence),body BLOB NOT NULL) STRICT;\n"),
    (16, "CREATE TABLE local_agent_sessions (
        target TEXT NOT NULL REFERENCES local_targets(target), session TEXT NOT NULL,
        nonce TEXT NOT NULL, epoch TEXT NOT NULL, current_handoff TEXT,
        phase TEXT NOT NULL CHECK(phase IN ('paused','running','unknown')),
        PRIMARY KEY(target,session)
    ) STRICT;
    CREATE TABLE local_handoffs (
        id TEXT PRIMARY KEY NOT NULL, target TEXT NOT NULL REFERENCES local_targets(target),
        session TEXT NOT NULL, operation TEXT NOT NULL, generation TEXT NOT NULL REFERENCES local_generations(id),
        body BLOB NOT NULL, state TEXT NOT NULL CHECK(state IN ('prepared','dispatching','adopted')),
        receipt TEXT, UNIQUE(target,session,operation)
    ) STRICT;
    CREATE TABLE local_agent_dispatches (
        id TEXT PRIMARY KEY NOT NULL, handoff TEXT NOT NULL REFERENCES local_handoffs(id),
        command BLOB NOT NULL, state TEXT NOT NULL CHECK(state IN ('intent','acknowledged','leader_completed','leader_completed_old','attached_old','unknown')),
        pid INTEGER, completion INTEGER
    ) STRICT;
    CREATE TABLE local_agent_reconciliations (
        id TEXT PRIMARY KEY NOT NULL, handoff TEXT NOT NULL REFERENCES local_handoffs(id), body BLOB NOT NULL
    ) STRICT;
    CREATE TABLE local_adoption_receipts (
        id TEXT PRIMARY KEY NOT NULL, handoff TEXT NOT NULL REFERENCES local_handoffs(id), body BLOB NOT NULL
    ) STRICT;"),
    (17,"CREATE TABLE local_imports (
        id TEXT PRIMARY KEY NOT NULL,bundle TEXT NOT NULL,target TEXT NOT NULL REFERENCES local_targets(target),
        epoch TEXT NOT NULL,body BLOB NOT NULL,header_sha256 TEXT NOT NULL,
        UNIQUE(bundle,target,epoch)
    ) STRICT;
    CREATE TABLE local_imported_sources (
        import_id TEXT NOT NULL REFERENCES local_imports(id),snapshot_id TEXT NOT NULL,PRIMARY KEY(import_id,snapshot_id)
    ) STRICT;
    CREATE TABLE local_imported_records (
        import_id TEXT NOT NULL REFERENCES local_imports(id),record_id TEXT NOT NULL,schema TEXT NOT NULL,object_sha256 TEXT NOT NULL,
        PRIMARY KEY(import_id,record_id)
    ) STRICT;
    CREATE TABLE local_import_rehearsals (
        id TEXT PRIMARY KEY NOT NULL,import_id TEXT NOT NULL REFERENCES local_imports(id),body BLOB NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('intent','prepared'))
    ) STRICT;"),

    (18, "CREATE TABLE IF NOT EXISTS local_native_gate_jobs (
        id TEXT PRIMARY KEY NOT NULL, wave TEXT NOT NULL UNIQUE REFERENCES local_waves(wave),
        target TEXT NOT NULL, revision TEXT NOT NULL, epoch TEXT NOT NULL, body BLOB NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('prepared','dispatching','completed','unknown')),
        observation BLOB
    ) STRICT;
    CREATE TABLE IF NOT EXISTS local_native_gate_roots (
        job TEXT NOT NULL REFERENCES local_native_gate_jobs(id),kind TEXT NOT NULL,
        reference TEXT NOT NULL,PRIMARY KEY(job,kind,reference)
    ) STRICT;"),
    (19, "CREATE TABLE IF NOT EXISTS local_controller_completions (
        id TEXT PRIMARY KEY NOT NULL, wave TEXT NOT NULL UNIQUE REFERENCES local_waves(wave),
        revision TEXT NOT NULL REFERENCES local_target_revisions(revision),
        target TEXT NOT NULL REFERENCES local_targets(target), body BLOB NOT NULL
    ) STRICT;
    CREATE TABLE IF NOT EXISTS local_controller_completion_roots (
        completion TEXT NOT NULL REFERENCES local_controller_completions(id),
        kind TEXT NOT NULL, reference TEXT NOT NULL,
        PRIMARY KEY(completion,kind,reference)
    ) STRICT;"),
    (20, "CREATE TABLE IF NOT EXISTS local_controller_runs (
        wave TEXT PRIMARY KEY NOT NULL REFERENCES local_waves(wave),
        id TEXT NOT NULL UNIQUE, epoch TEXT NOT NULL, body BLOB NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('dispatching','completed','held','unknown')),
        observation BLOB
    ) STRICT;"),
    (21, "SELECT 1;"),
    (22, "CREATE TABLE IF NOT EXISTS local_controller_services (
        id TEXT PRIMARY KEY NOT NULL,target TEXT NOT NULL REFERENCES local_targets(target),
        revision TEXT NOT NULL REFERENCES local_target_revisions(revision),epoch TEXT NOT NULL,
        nonce TEXT NOT NULL,body BLOB NOT NULL,state TEXT NOT NULL CHECK(state IN ('dispatching','completed','held')),
        report BLOB,UNIQUE(target,revision,epoch,nonce)
    ) STRICT;"),
    (23, "ALTER TABLE local_native_gate_jobs ADD COLUMN roots_released_ms INTEGER;
    CREATE TRIGGER local_native_gate_no_dispatch_after_root_release
    BEFORE UPDATE OF state ON local_native_gate_jobs
    WHEN OLD.roots_released_ms IS NOT NULL AND NEW.state<>OLD.state
    BEGIN SELECT RAISE(ABORT, 'native gate roots were released'); END;"),
    (24, "CREATE TABLE local_sensitive_incidents (
        id TEXT PRIMARY KEY NOT NULL,
        project_key TEXT NOT NULL,
        issuer_label TEXT NOT NULL,
        operator_uid INTEGER NOT NULL CHECK(operator_uid >= 0),
        authority_epoch TEXT NOT NULL,
        scope_kind TEXT NOT NULL CHECK(scope_kind='project'),
        scope_id TEXT NOT NULL,
        reason_code TEXT NOT NULL CHECK(reason_code IN
            ('suspected_test_secret_exposure','misrouted_context','operator_requested_purge')),
        request_digest TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('quarantined','purging','purged')),
        inventory_digest TEXT,
        database_cleanup_completed_ms INTEGER,
        created_ms INTEGER NOT NULL,
        updated_ms INTEGER NOT NULL,
        UNIQUE(scope_kind, scope_id)
    ) STRICT;
    CREATE TABLE local_sensitive_items (
        incident_id TEXT NOT NULL REFERENCES local_sensitive_incidents(id),
        relative_path TEXT NOT NULL,
        sha256 TEXT NOT NULL,
        bytes INTEGER NOT NULL CHECK(bytes >= 0),
        state TEXT NOT NULL CHECK(state IN ('pending','deleted','residual')),
        PRIMARY KEY(incident_id, relative_path)
    ) STRICT;
    CREATE TABLE local_sensitive_residuals (
        incident_id TEXT NOT NULL REFERENCES local_sensitive_incidents(id),
        code TEXT NOT NULL,
        relative_path TEXT NOT NULL DEFAULT '',
        permanence TEXT NOT NULL CHECK(permanence IN ('transient','unmanaged','unclassified')),
        resolved_ms INTEGER,
        PRIMARY KEY(incident_id, code, relative_path)
    ) STRICT;
    CREATE TABLE local_sensitive_exports (
        path TEXT PRIMARY KEY NOT NULL,
        kind TEXT NOT NULL CHECK(kind IN ('export','authority_history')),
        sha256 TEXT NOT NULL,
        bytes INTEGER NOT NULL CHECK(bytes >= 0),
        state TEXT NOT NULL CHECK(state IN ('intent','present')),
        created_ms INTEGER NOT NULL
    ) STRICT;
    CREATE TABLE local_sensitive_table_inventory (
        incident_id TEXT NOT NULL REFERENCES local_sensitive_incidents(id),
        table_name TEXT NOT NULL,
        row_count INTEGER NOT NULL CHECK(row_count >= 0),
        bytes INTEGER NOT NULL CHECK(bytes >= 0),
        sha256 TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('pending','cleared')),
        PRIMARY KEY(incident_id,table_name)
    ) STRICT;
    CREATE TRIGGER local_sensitive_incident_no_delete
    BEFORE DELETE ON local_sensitive_incidents
    BEGIN SELECT RAISE(ABORT, 'sensitive-response tombstones are permanent'); END;
    CREATE TRIGGER local_sensitive_incident_scope_immutable
    BEFORE UPDATE OF id,project_key,issuer_label,operator_uid,authority_epoch,
        scope_kind,scope_id,reason_code,request_digest,created_ms
    ON local_sensitive_incidents
    BEGIN SELECT RAISE(ABORT, 'sensitive-response scope is immutable'); END;
    CREATE TRIGGER local_sensitive_incident_state_monotone
    BEFORE UPDATE OF state ON local_sensitive_incidents
    WHEN NOT ((OLD.state='quarantined' AND NEW.state IN ('quarantined','purging')) OR
        (OLD.state='purging' AND NEW.state IN ('quarantined','purging','purged')) OR
        (OLD.state='purged' AND NEW.state='purged'))
    BEGIN SELECT RAISE(ABORT, 'sensitive-response state cannot be reopened'); END;
    CREATE TRIGGER local_sensitive_item_scope_immutable
    BEFORE UPDATE OF incident_id,relative_path,sha256,bytes ON local_sensitive_items
    BEGIN SELECT RAISE(ABORT, 'sensitive-response inventory is immutable'); END;
    CREATE TRIGGER local_sensitive_item_no_delete
    BEFORE DELETE ON local_sensitive_items
    BEGIN SELECT RAISE(ABORT, 'sensitive-response inventory is permanent'); END;
    CREATE TRIGGER local_sensitive_inventory_digest_immutable
    BEFORE UPDATE OF inventory_digest ON local_sensitive_incidents
    WHEN OLD.inventory_digest IS NOT NULL AND NEW.inventory_digest IS NOT OLD.inventory_digest
    BEGIN SELECT RAISE(ABORT, 'sensitive-response inventory digest is immutable'); END;
    CREATE TRIGGER local_sensitive_item_state_monotone
    BEFORE UPDATE OF state ON local_sensitive_items
    WHEN NOT (OLD.state=NEW.state OR (OLD.state='pending' AND NEW.state IN ('deleted','residual')))
    BEGIN SELECT RAISE(ABORT, 'sensitive-response item state cannot be reopened'); END;
    CREATE TRIGGER local_sensitive_residual_scope_immutable
    BEFORE UPDATE OF incident_id,code,relative_path,permanence ON local_sensitive_residuals
    BEGIN SELECT RAISE(ABORT, 'sensitive-response residual identity is immutable'); END;
    CREATE TRIGGER local_sensitive_residual_no_delete
    BEFORE DELETE ON local_sensitive_residuals
    BEGIN SELECT RAISE(ABORT, 'sensitive-response residual history is permanent'); END;
    CREATE TRIGGER local_sensitive_export_no_delete
    BEFORE DELETE ON local_sensitive_exports
    BEGIN SELECT RAISE(ABORT, 'sensitive-response export inventory is permanent'); END;
    CREATE TRIGGER local_sensitive_export_identity_immutable
    BEFORE UPDATE OF path,kind,sha256,bytes,created_ms ON local_sensitive_exports
    BEGIN SELECT RAISE(ABORT, 'sensitive-response export identity is immutable'); END;
    CREATE TRIGGER local_sensitive_export_state_monotone
    BEFORE UPDATE OF state ON local_sensitive_exports
    WHEN NOT (OLD.state=NEW.state OR (OLD.state='intent' AND NEW.state='present'))
    BEGIN SELECT RAISE(ABORT, 'sensitive-response export state cannot be reopened'); END;
    CREATE TRIGGER local_sensitive_table_inventory_identity_immutable
    BEFORE UPDATE OF incident_id,table_name,row_count,bytes,sha256
    ON local_sensitive_table_inventory
    BEGIN SELECT RAISE(ABORT, 'sensitive-response table inventory is immutable'); END;
    CREATE TRIGGER local_sensitive_table_inventory_no_delete
    BEFORE DELETE ON local_sensitive_table_inventory
    BEGIN SELECT RAISE(ABORT, 'sensitive-response table inventory is permanent'); END;
    CREATE TRIGGER local_sensitive_table_inventory_state_monotone
    BEFORE UPDATE OF state ON local_sensitive_table_inventory
    WHEN NOT (OLD.state=NEW.state OR (OLD.state='pending' AND NEW.state='cleared'))
    BEGIN SELECT RAISE(ABORT, 'sensitive-response table inventory state cannot be reopened'); END;
    CREATE TRIGGER local_sensitive_database_cleanup_receipt_immutable
    BEFORE UPDATE OF database_cleanup_completed_ms ON local_sensitive_incidents
    WHEN OLD.database_cleanup_completed_ms IS NOT NULL
        OR NEW.database_cleanup_completed_ms IS NULL
    BEGIN SELECT RAISE(ABORT, 'sensitive-response database cleanup receipt is immutable'); END;"),
    (25, "CREATE TABLE local_capture_notices (
        notice_id TEXT PRIMARY KEY NOT NULL,
        target TEXT NOT NULL REFERENCES local_targets(target),
        revision TEXT NOT NULL REFERENCES local_target_revisions(revision),
        operation_id TEXT NOT NULL REFERENCES capture_operations(operation_id),
        alias TEXT NOT NULL,
        lineage_record_id TEXT NOT NULL,
        receipt_record_id TEXT NOT NULL,
        authority_epoch TEXT NOT NULL,
        base_snapshot TEXT NOT NULL,
        base_commit TEXT NOT NULL,
        result_snapshot TEXT NOT NULL,
        result_commit TEXT NOT NULL,
        received_ms INTEGER NOT NULL CHECK(received_ms >= 0),
        body BLOB NOT NULL,
        UNIQUE(target,revision,operation_id,alias,authority_epoch)
    ) STRICT;
    CREATE INDEX local_capture_notices_pending
        ON local_capture_notices(target,received_ms,notice_id);
    CREATE TABLE local_capture_notice_outcomes (
        notice_id TEXT PRIMARY KEY NOT NULL REFERENCES local_capture_notices(notice_id),
        status TEXT NOT NULL CHECK(status IN ('pending','available','stale')),
        detail TEXT,
        changed_ms INTEGER NOT NULL CHECK(changed_ms >= 0)
    ) STRICT;
    CREATE TRIGGER local_capture_notice_outcome_monotone
    BEFORE UPDATE OF status ON local_capture_notice_outcomes
    WHEN NOT ((OLD.status='pending' AND NEW.status IN ('pending','available','stale')) OR
        (OLD.status='available' AND NEW.status='available') OR
        (OLD.status='stale' AND NEW.status='stale'))
    BEGIN SELECT RAISE(ABORT, 'capture notice outcome cannot be reopened'); END;"),
            (26, "ALTER TABLE contribution_briefs ADD COLUMN revision_record_id TEXT;
    CREATE UNIQUE INDEX capture_receipts_operation_lineage
        ON capture_receipts(operation_id,lineage_record_id);
    CREATE TABLE local_logical_contributions (
        operation_id TEXT PRIMARY KEY NOT NULL REFERENCES capture_operations(operation_id),
        contribution_id TEXT NOT NULL UNIQUE,
        lineage_record_id TEXT NOT NULL,
        project_id TEXT NOT NULL,
        created_ms INTEGER NOT NULL,
        UNIQUE (operation_id, contribution_id, lineage_record_id),
        FOREIGN KEY (operation_id, lineage_record_id)
            REFERENCES capture_receipts(operation_id, lineage_record_id)
    ) STRICT;
    CREATE INDEX local_logical_contributions_by_lineage
        ON local_logical_contributions(lineage_record_id, contribution_id);
    CREATE TABLE contribution_revisions (
        revision_record_id TEXT PRIMARY KEY NOT NULL,
        operation_id TEXT NOT NULL,
        contribution_id TEXT NOT NULL,
        lineage_record_id TEXT NOT NULL,
        record_sha256 TEXT NOT NULL,
        brief_record_id TEXT,
        brief_object_sha256 TEXT,
        created_ms INTEGER NOT NULL,
        CHECK ((brief_record_id IS NULL) = (brief_object_sha256 IS NULL)),
        UNIQUE (contribution_id, revision_record_id),
        FOREIGN KEY (operation_id, contribution_id, lineage_record_id)
            REFERENCES local_logical_contributions(operation_id, contribution_id, lineage_record_id)
    ) STRICT;
    CREATE INDEX contribution_revisions_by_lineage
        ON contribution_revisions(lineage_record_id, created_ms, revision_record_id);
    CREATE INDEX contribution_revisions_by_contribution
        ON contribution_revisions(contribution_id, created_ms, revision_record_id);
    CREATE INDEX contribution_revisions_by_object
        ON contribution_revisions(record_sha256, revision_record_id);
    CREATE TABLE reclaimed_contribution_revisions (
        revision_record_id TEXT PRIMARY KEY NOT NULL,
        generation INTEGER NOT NULL REFERENCES gc_generations(generation),
        reclaimed_ms INTEGER NOT NULL CHECK(reclaimed_ms >= 0)
    ) STRICT;
    CREATE INDEX reclaimed_contribution_revisions_by_generation
        ON reclaimed_contribution_revisions(generation, revision_record_id);
    CREATE TABLE contribution_revision_identity_bases (
        revision_record_id TEXT NOT NULL
            REFERENCES contribution_revisions(revision_record_id),
        base_revision_record_id TEXT NOT NULL
            REFERENCES contribution_revisions(revision_record_id),
        PRIMARY KEY (revision_record_id, base_revision_record_id),
        CHECK (revision_record_id <> base_revision_record_id)
    ) STRICT;
    CREATE INDEX contribution_revision_identity_bases_by_base
        ON contribution_revision_identity_bases(base_revision_record_id, revision_record_id);
    CREATE TABLE contribution_revision_heads (
        contribution_id TEXT PRIMARY KEY NOT NULL
            REFERENCES local_logical_contributions(contribution_id),
        revision_record_id TEXT NOT NULL,
        FOREIGN KEY (contribution_id, revision_record_id)
            REFERENCES contribution_revisions(contribution_id, revision_record_id)
    ) STRICT;
    CREATE TABLE local_project_bindings (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        project_id TEXT NOT NULL,
        descriptor_record_id TEXT NOT NULL,
        descriptor_object_sha256 TEXT NOT NULL,
        anchor_commit TEXT NOT NULL,
        anchor_snapshot TEXT NOT NULL,
        confirmed_descriptor_record_id TEXT NOT NULL,
        committed_config_commit TEXT NOT NULL,
        bootstrap_snapshot TEXT NOT NULL REFERENCES retained_snapshots(snapshot_id),
        bootstrap_transition_sha256 TEXT NOT NULL
            CHECK (length(bootstrap_transition_sha256) = 64),
        bound_ms INTEGER NOT NULL,
        CHECK (descriptor_record_id = confirmed_descriptor_record_id)
    ) STRICT;
    CREATE TABLE local_project_file_ids (
        snapshot_id TEXT NOT NULL REFERENCES retained_snapshots(snapshot_id),
        path BLOB NOT NULL,
        file_id TEXT NOT NULL,
        mode TEXT NOT NULL,
        content_sha256 TEXT NOT NULL,
        PRIMARY KEY (snapshot_id, path),
        UNIQUE (snapshot_id, file_id)
    ) STRICT;
    CREATE INDEX local_project_file_ids_by_snapshot
        ON local_project_file_ids(snapshot_id);"),
    (
        27,
        "ALTER TABLE resolution_attempts ADD COLUMN async_token TEXT;
         ALTER TABLE resolution_attempts ADD COLUMN client_key TEXT;
         ALTER TABLE resolution_attempts ADD COLUMN session_label TEXT;
         ALTER TABLE resolution_attempts ADD COLUMN attempt_number INTEGER;
         ALTER TABLE resolution_attempts ADD COLUMN request_sha256 TEXT;
         ALTER TABLE resolution_attempts ADD COLUMN input_sha256 TEXT;
         ALTER TABLE resolution_attempts ADD COLUMN input_manifest_sha256 TEXT;
         ALTER TABLE resolution_attempts ADD COLUMN dispatch_limits TEXT;
         ALTER TABLE resolution_attempts ADD COLUMN response_sha256 TEXT;
         ALTER TABLE resolution_attempts ADD COLUMN result_record_id TEXT;
         ALTER TABLE resolution_attempts ADD COLUMN result_sha256 TEXT;
         ALTER TABLE resolution_attempts ADD COLUMN detail TEXT;
         ALTER TABLE resolution_attempts ADD COLUMN async_state TEXT NOT NULL DEFAULT 'settled'
             CHECK (async_state IN ('settled','exported','dispatching','outcome_unknown','importing'));
         ALTER TABLE local_waves ADD COLUMN resolution_request_record_id TEXT;
         ALTER TABLE local_waves ADD COLUMN resolution_request_object TEXT;
         ALTER TABLE local_waves ADD COLUMN composition_source_record_id TEXT;
         ALTER TABLE local_waves ADD COLUMN composition_source_object TEXT;
         UPDATE resolution_attempts
             SET async_state = CASE WHEN outcome IS NULL THEN 'outcome_unknown' ELSE 'settled' END;
         CREATE UNIQUE INDEX resolution_attempts_async_token
             ON resolution_attempts(async_token) WHERE async_token IS NOT NULL;
         CREATE UNIQUE INDEX resolution_attempts_session_client_key
             ON resolution_attempts(session_label, client_key)
             WHERE session_label IS NOT NULL AND client_key IS NOT NULL;
         -- Already-open pre-v27 writers omit attempt_number. Once the new
         -- version is stamped, refuse their later charge or settlement SQL.
         -- Legacy rows remain charged and read-only; null outcomes were
         -- classified as unknown above and require a future strict reconcile.
         CREATE TRIGGER resolution_attempts_require_v27_charge
         BEFORE INSERT ON resolution_attempts
         WHEN (NEW.attempt_number IS NULL OR NEW.attempt_number < 1
               OR NEW.async_state = 'settled'
               OR (NEW.async_token IS NOT NULL AND
                   (NEW.async_state != 'exported'
                    OR NEW.client_key IS NULL OR NEW.session_label IS NULL
                    OR NEW.request_sha256 IS NULL OR NEW.input_sha256 IS NULL
                    OR NEW.input_manifest_sha256 IS NULL OR NEW.dispatch_limits IS NULL)))
          AND COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key='schema_version'), 0) >= 27
         BEGIN
             SELECT RAISE(ABORT, 'resolution attempt requires a v27 pre-dispatch charge');
         END;
         CREATE TRIGGER resolution_attempts_legacy_rows_immutable
         BEFORE UPDATE ON resolution_attempts
         WHEN OLD.attempt_number IS NULL
          AND COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key='schema_version'), 0) >= 27
         BEGIN
             SELECT RAISE(ABORT, 'legacy charged resolution attempts are immutable');
         END;
         CREATE TRIGGER resolution_attempts_never_delete
         BEFORE DELETE ON resolution_attempts
         BEGIN
             SELECT RAISE(ABORT, 'charged resolution attempts cannot be deleted');
         END;
         CREATE TRIGGER resolution_attempts_identity_immutable
         BEFORE UPDATE OF decision_key,kind,members,paths,request_record_id,charged_ms,
                          async_token,client_key,session_label,attempt_number,request_sha256,
                          input_sha256,input_manifest_sha256,dispatch_limits
         ON resolution_attempts
         WHEN NEW.decision_key IS NOT OLD.decision_key
           OR NEW.kind IS NOT OLD.kind
           OR NEW.members IS NOT OLD.members
           OR NEW.paths IS NOT OLD.paths
           OR NEW.request_record_id IS NOT OLD.request_record_id
           OR NEW.charged_ms IS NOT OLD.charged_ms
           OR NEW.async_token IS NOT OLD.async_token
           OR NEW.client_key IS NOT OLD.client_key
           OR NEW.session_label IS NOT OLD.session_label
           OR NEW.attempt_number IS NOT OLD.attempt_number
           OR NEW.request_sha256 IS NOT OLD.request_sha256
           OR NEW.input_sha256 IS NOT OLD.input_sha256
           OR NEW.input_manifest_sha256 IS NOT OLD.input_manifest_sha256
           OR NEW.dispatch_limits IS NOT OLD.dispatch_limits
         BEGIN
             SELECT RAISE(ABORT, 'resolution attempt charge identity is immutable');
         END;
         -- A stale pre-v27 resolver can still hold a connection across the
         -- migration. Its old settlement statement updates only `outcome`;
         -- reject that write on new rows too. Current writers must advance
         -- through the charged state machine and bind one response digest.
         CREATE TRIGGER resolution_attempts_lifecycle_guard
         BEFORE UPDATE ON resolution_attempts
         WHEN OLD.attempt_number IS NOT NULL
          AND (
             (NEW.async_state = OLD.async_state AND (
                 NEW.outcome IS NOT OLD.outcome
                 OR NEW.response_sha256 IS NOT OLD.response_sha256
                 OR NEW.result_record_id IS NOT OLD.result_record_id
                 OR NEW.result_sha256 IS NOT OLD.result_sha256
                 OR NEW.detail IS NOT OLD.detail
             ))
             OR (NEW.async_state != OLD.async_state AND NOT (
                 (OLD.async_state = 'exported' AND NEW.async_state IN ('dispatching','outcome_unknown'))
                 OR (OLD.async_state = 'dispatching' AND NEW.async_state IN ('outcome_unknown','importing'))
                 OR (OLD.async_state = 'dispatching' AND NEW.async_state = 'settled'
                     AND OLD.async_token IS NULL)
                 OR (OLD.async_state = 'outcome_unknown' AND NEW.async_state = 'importing')
                 OR (OLD.async_state = 'importing' AND NEW.async_state = 'settled')
             ))
             OR (NEW.async_state = 'importing' AND NEW.response_sha256 IS NULL)
             OR (OLD.response_sha256 IS NOT NULL
                 AND NEW.response_sha256 IS NOT OLD.response_sha256)
             OR (NEW.async_state = 'settled' AND NEW.outcome IS NULL)
             OR (NEW.async_state != 'settled' AND NEW.outcome IS NOT OLD.outcome)
             OR (NEW.async_state != 'settled' AND (
                 NEW.result_record_id IS NOT OLD.result_record_id
                 OR NEW.result_sha256 IS NOT OLD.result_sha256
             ))
             OR ((NEW.result_record_id IS NULL) != (NEW.result_sha256 IS NULL))
             OR (NEW.async_state = 'importing'
                 AND OLD.response_sha256 IS NOT NULL
                 AND NEW.response_sha256 IS NOT OLD.response_sha256)
             OR (NEW.async_state != 'importing'
                 AND OLD.response_sha256 IS NULL
                 AND NEW.response_sha256 IS NOT NULL)
          )
         BEGIN
             SELECT RAISE(ABORT, 'resolution attempt lifecycle transition is invalid');
         END;
         CREATE TRIGGER local_waves_resolution_request_pair_insert
         BEFORE INSERT ON local_waves
         WHEN (NEW.resolution_request_record_id IS NULL)
           != (NEW.resolution_request_object IS NULL)
         BEGIN
             SELECT RAISE(ABORT, 'resolution request reference must be complete');
         END;
         CREATE TRIGGER local_waves_resolution_request_pair_update
         BEFORE UPDATE OF resolution_request_record_id,resolution_request_object
         ON local_waves
         WHEN (NEW.resolution_request_record_id IS NULL)
           != (NEW.resolution_request_object IS NULL)
         BEGIN
             SELECT RAISE(ABORT, 'resolution request reference must be complete');
         END;
         CREATE TRIGGER local_waves_composition_source_pair_insert
         BEFORE INSERT ON local_waves
         WHEN (NEW.composition_source_record_id IS NULL)
           != (NEW.composition_source_object IS NULL)
         BEGIN
             SELECT RAISE(ABORT, 'composition source reference must be complete');
         END;
         CREATE TRIGGER local_waves_composition_source_pair_update
         BEFORE UPDATE OF composition_source_record_id,composition_source_object
         ON local_waves
         WHEN (NEW.composition_source_record_id IS NULL)
           != (NEW.composition_source_object IS NULL)
         BEGIN
             SELECT RAISE(ABORT, 'composition source reference must be complete');
         END;",
    ),
    (
        28,
        r#"ALTER TABLE local_waves ADD COLUMN resolution_result_record_id TEXT;
         ALTER TABLE local_waves ADD COLUMN resolution_result_object TEXT;
         CREATE TRIGGER local_waves_resolution_result_pair_insert
         BEFORE INSERT ON local_waves
         WHEN ((NEW.resolution_result_record_id IS NULL)
               != (NEW.resolution_result_object IS NULL))
           OR (NEW.resolution_result_record_id IS NOT NULL
               AND (NEW.resolution_request_record_id IS NULL
                    OR NEW.resolution_request_object IS NULL))
         BEGIN
             SELECT RAISE(ABORT, 'resolution result reference must be complete and linked to a request');
         END;
         CREATE TRIGGER local_waves_resolution_result_pair_update
         BEFORE UPDATE OF resolution_request_record_id,resolution_request_object,
                          resolution_result_record_id,resolution_result_object,
                          candidate_snapshot,candidate_commit
         ON local_waves
         WHEN ((NEW.resolution_result_record_id IS NULL)
               != (NEW.resolution_result_object IS NULL))
           OR (NEW.resolution_result_record_id IS NOT NULL
               AND (NEW.resolution_request_record_id IS NULL
                    OR NEW.resolution_request_object IS NULL))
           OR (OLD.resolution_result_record_id IS NOT NULL
               AND (NEW.resolution_request_record_id IS NOT OLD.resolution_request_record_id
                    OR NEW.resolution_request_object IS NOT OLD.resolution_request_object
                    OR NEW.resolution_result_record_id IS NOT OLD.resolution_result_record_id
                    OR NEW.resolution_result_object IS NOT OLD.resolution_result_object
                    OR NEW.candidate_snapshot IS NOT OLD.candidate_snapshot
                    OR NEW.candidate_commit IS NOT OLD.candidate_commit))
         BEGIN
             SELECT RAISE(ABORT, 'resolution result provenance is immutable once attached');
         END;

         DROP TRIGGER IF EXISTS local_native_gate_no_dispatch_after_root_release;
         CREATE TABLE local_native_gate_jobs_v28 (
             id TEXT PRIMARY KEY NOT NULL,
             wave TEXT NOT NULL REFERENCES local_waves(wave),
             target TEXT NOT NULL, revision TEXT NOT NULL, epoch TEXT NOT NULL,
             body BLOB NOT NULL,
             state TEXT NOT NULL CHECK(state IN ('prepared','dispatching','completed','unknown')),
             observation BLOB,
             roots_released_ms INTEGER
         ) STRICT;
         CREATE TABLE local_native_gate_roots_v28 (
             job TEXT NOT NULL REFERENCES local_native_gate_jobs_v28(id),
             kind TEXT NOT NULL, reference TEXT NOT NULL,
             PRIMARY KEY(job,kind,reference)
         ) STRICT;
         INSERT INTO local_native_gate_jobs_v28
             (id,wave,target,revision,epoch,body,state,observation,roots_released_ms)
             SELECT id,wave,target,revision,epoch,body,state,observation,roots_released_ms
             FROM local_native_gate_jobs;
         INSERT INTO local_native_gate_roots_v28 (job,kind,reference)
             SELECT job,kind,reference FROM local_native_gate_roots;
         DROP TABLE local_native_gate_roots;
         DROP TABLE local_native_gate_jobs;
         ALTER TABLE local_native_gate_jobs_v28 RENAME TO local_native_gate_jobs;
         ALTER TABLE local_native_gate_roots_v28 RENAME TO local_native_gate_roots;
         CREATE INDEX local_native_gate_jobs_by_wave
             ON local_native_gate_jobs(wave,id);
         CREATE TRIGGER local_native_gate_no_dispatch_after_root_release
         BEFORE UPDATE OF state ON local_native_gate_jobs
         WHEN OLD.roots_released_ms IS NOT NULL AND NEW.state<>OLD.state
         BEGIN SELECT RAISE(ABORT, 'native gate roots were released'); END;"#,
    ),
    (
        29,
        r#"CREATE TABLE local_resolution_reservation_proofs (
             reservation TEXT PRIMARY KEY NOT NULL
                 REFERENCES local_budget_reservations(reservation),
             target TEXT NOT NULL,
             issuer TEXT NOT NULL,
             authority_epoch TEXT NOT NULL,
             client_key TEXT NOT NULL CHECK(length(client_key) BETWEEN 1 AND 256),
             wave TEXT NOT NULL REFERENCES local_waves(wave),
             expected_version INTEGER NOT NULL CHECK(expected_version >= 0),
             request_record_id TEXT NOT NULL,
             request_object_sha256 TEXT NOT NULL CHECK(length(request_object_sha256)=64),
             composition_source_record_id TEXT NOT NULL,
             composition_source_object_sha256 TEXT NOT NULL
                 CHECK(length(composition_source_object_sha256)=64),
             -- ResolutionInputManifest is archived as canonical bytes and
             -- identified by ObjectDigest; it is not a RecordRef.
             input_manifest_object_sha256 TEXT NOT NULL
                 CHECK(length(input_manifest_object_sha256)=64),
             trigger_sha256 TEXT NOT NULL CHECK(length(trigger_sha256)=64),
             project_policy TEXT NOT NULL,
             objective_policy TEXT NOT NULL,
             price_profile TEXT NOT NULL,
             dispatch_limits TEXT NOT NULL CHECK(length(dispatch_limits) BETWEEN 1 AND 512),
             reserved_envelope BLOB NOT NULL CHECK(length(reserved_envelope) BETWEEN 1 AND 4096),
             reservation_attempt_limit INTEGER NOT NULL CHECK(reservation_attempt_limit=1),
             deadline_ms INTEGER NOT NULL CHECK(deadline_ms >= 0),
             created_ms INTEGER NOT NULL CHECK(created_ms >= 0),
             UNIQUE(target,issuer,client_key)
         ) STRICT;
         CREATE TABLE local_resolution_reservation_roots (
             reservation TEXT NOT NULL
                 REFERENCES local_resolution_reservation_proofs(reservation),
             lineage_record_id TEXT NOT NULL,
             PRIMARY KEY(reservation,lineage_record_id)
         ) STRICT;
         CREATE TABLE local_managed_resolution_attempt_bindings (
             binding_id TEXT PRIMARY KEY NOT NULL,
             reservation TEXT NOT NULL UNIQUE
                 REFERENCES local_resolution_reservation_proofs(reservation),
             attempt_id INTEGER NOT NULL UNIQUE
                 REFERENCES resolution_attempts(attempt_id),
             authority_epoch TEXT NOT NULL,
             reservation_version INTEGER NOT NULL CHECK(reservation_version >= 1),
             shared_attempt_number INTEGER NOT NULL CHECK(shared_attempt_number >= 1),
             reservation_attempt_number INTEGER NOT NULL
                 CHECK(reservation_attempt_number=1),
             route TEXT NOT NULL
                 CHECK(route='local_managed_resolution_v1'),
             created_ms INTEGER NOT NULL CHECK(created_ms >= 0)
         ) STRICT;
         CREATE TRIGGER local_resolution_reservation_proof_scope
         BEFORE INSERT ON local_resolution_reservation_proofs
         WHEN NOT EXISTS (
             SELECT 1
             FROM local_budget_reservations r
             JOIN local_waves w ON w.wave=NEW.wave
             JOIN local_targets t ON t.target=r.target
             WHERE r.reservation=NEW.reservation AND r.target=NEW.target
               AND r.wave=NEW.wave AND r.parent IS NULL
               AND w.wave=r.wave AND w.target=NEW.target
               AND w.revision=r.revision
               AND t.issuer=NEW.issuer
               AND t.active_revision=r.revision AND t.version=NEW.expected_version
         )
         BEGIN
             SELECT RAISE(ABORT, 'managed resolution proof must match a root reservation');
         END;
         CREATE TRIGGER local_resolution_reservation_proof_immutable
         BEFORE UPDATE ON local_resolution_reservation_proofs
         BEGIN
             SELECT RAISE(ABORT, 'managed resolution reservation proof is immutable');
         END;
         CREATE TRIGGER local_resolution_reservation_proof_no_delete
         BEFORE DELETE ON local_resolution_reservation_proofs
         BEGIN
             SELECT RAISE(ABORT, 'managed resolution reservation proof is permanent');
         END;
         CREATE TRIGGER local_resolution_reservation_root_immutable
         BEFORE UPDATE ON local_resolution_reservation_roots
         BEGIN
             SELECT RAISE(ABORT, 'managed resolution reservation roots are immutable');
         END;
         CREATE TRIGGER local_resolution_reservation_root_no_delete
         BEFORE DELETE ON local_resolution_reservation_roots
         BEGIN
             SELECT RAISE(ABORT, 'managed resolution reservation roots are permanent');
         END;
         CREATE TRIGGER local_resolution_reservation_root_before_binding
         BEFORE INSERT ON local_resolution_reservation_roots
         WHEN EXISTS (
             SELECT 1 FROM local_managed_resolution_attempt_bindings b
             WHERE b.reservation=NEW.reservation
         )
         BEGIN
             SELECT RAISE(ABORT, 'managed resolution roots are closed by attempt binding');
         END;
         CREATE TRIGGER local_managed_resolution_binding_matches_attempt
         BEFORE INSERT ON local_managed_resolution_attempt_bindings
         WHEN NOT EXISTS (
             SELECT 1
             FROM local_resolution_reservation_proofs p
             JOIN local_budget_reservations r ON r.reservation=p.reservation
             JOIN local_waves w ON w.wave=p.wave
             JOIN local_targets t ON t.target=p.target AND t.issuer=p.issuer
             JOIN resolution_attempts a ON a.attempt_id=NEW.attempt_id
             WHERE p.reservation=NEW.reservation
               AND p.authority_epoch=NEW.authority_epoch
               AND r.target=p.target AND r.wave=p.wave
               AND w.target=p.target AND w.revision=r.revision
               AND t.active_revision=r.revision AND t.version=p.expected_version
               AND w.resolution_request_record_id=p.request_record_id
               AND w.resolution_request_object=p.request_object_sha256
               AND w.composition_source_record_id=p.composition_source_record_id
               AND w.composition_source_object=p.composition_source_object_sha256
               AND r.phase='completion_pending'
               AND r.version=NEW.reservation_version
               AND r.attempts=NEW.reservation_attempt_number
               AND EXISTS (
                   SELECT 1 FROM local_resolution_reservation_roots rr
                   WHERE rr.reservation=p.reservation
               )
               AND a.request_record_id=p.request_record_id
               AND a.request_sha256=p.request_object_sha256
               AND a.input_manifest_sha256=p.input_manifest_object_sha256
               AND a.client_key=p.client_key
               AND a.resolver='local-managed-resolution'
               AND a.session_label IS NULL
               AND a.attempt_number=NEW.shared_attempt_number
         )
         BEGIN
             SELECT RAISE(ABORT, 'managed resolution binding does not match its charged attempt');
         END;
         CREATE TRIGGER local_managed_resolution_binding_immutable
         BEFORE UPDATE ON local_managed_resolution_attempt_bindings
         BEGIN
             SELECT RAISE(ABORT, 'managed resolution attempt binding is immutable');
         END;
         CREATE TRIGGER local_managed_resolution_binding_no_delete
         BEFORE DELETE ON local_managed_resolution_attempt_bindings
         BEGIN
             SELECT RAISE(ABORT, 'managed resolution attempt binding is permanent');
         END;"#,
    ),
];
/// Reconstruct the exact pre-schema-26 shape in historical migration fixtures
/// while preserving all schema-25 rows. Fixtures start from a current store,
/// so also remove schema-29, schema-28 and schema-27 additions before
/// lowering their version marker;
/// otherwise they model an impossible hybrid database and make the migration
/// add columns that the fixture already has. This is test setup only:
/// production migrations continue to reject malformed databases instead of
/// repairing them.
#[cfg(test)]
pub(crate) fn drop_identity_revision_schema_for_legacy_fixture(
    connection: &rusqlite::Connection,
) -> rusqlite::Result<()> {
    drop_resolution_schema_27_for_legacy_fixture(connection)?;
    connection.execute_batch(
        "DROP TABLE IF EXISTS contribution_revision_heads;
         DROP TABLE IF EXISTS contribution_revision_identity_bases;
         DROP TABLE IF EXISTS reclaimed_contribution_revisions;
         DROP TABLE IF EXISTS contribution_revisions;
         DROP TABLE IF EXISTS local_logical_contributions;
         DROP TABLE IF EXISTS local_project_file_ids;
         DROP TABLE IF EXISTS local_project_bindings;
         DROP INDEX IF EXISTS capture_receipts_operation_lineage;",
    )?;

    let has_briefs: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master
             WHERE type='table' AND name='contribution_briefs'
         )",
        [],
        |row| row.get(0),
    )?;
    if has_briefs {
        let has_revision_column = {
            let mut statement = connection.prepare("PRAGMA table_info(contribution_briefs)")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
            let mut found = false;
            for column in rows {
                if column? == "revision_record_id" {
                    found = true;
                    break;
                }
            }
            found
        };
        if has_revision_column {
            connection
                .execute_batch("ALTER TABLE contribution_briefs DROP COLUMN revision_record_id;")?;
        }
    }
    Ok(())
}

/// Remove only schema-28 wave result references from a current test store.
/// Refuse before dropping triggers or columns if a result has been attached, so
/// a historical fixture cannot silently erase current provenance.
#[cfg(test)]
fn drop_resolution_schema_28_for_legacy_fixture(
    connection: &rusqlite::Connection,
) -> rusqlite::Result<()> {
    drop_resolution_schema_29_for_legacy_fixture(connection)?;
    if !fixture_table_exists(connection, "local_waves")? {
        return Ok(());
    }
    let has_result_id =
        fixture_column_exists(connection, "local_waves", "resolution_result_record_id")?;
    let has_result_object =
        fixture_column_exists(connection, "local_waves", "resolution_result_object")?;
    assert_eq!(
        has_result_id, has_result_object,
        "cannot downgrade a fixture with an incomplete schema-28 result pair"
    );
    if !has_result_id {
        return Ok(());
    }

    let has_v28_result_data: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM local_waves
             WHERE resolution_result_record_id IS NOT NULL
                OR resolution_result_object IS NOT NULL
         )",
        [],
        |row| row.get(0),
    )?;
    assert!(
        !has_v28_result_data,
        "cannot downgrade a fixture containing schema-28 resolution-result data"
    );

    connection.execute_batch(
        "DROP TRIGGER IF EXISTS local_waves_resolution_result_pair_insert;
         DROP TRIGGER IF EXISTS local_waves_resolution_result_pair_update;",
    )?;
    connection.execute_batch(
        "ALTER TABLE local_waves DROP COLUMN resolution_result_object;
         ALTER TABLE local_waves DROP COLUMN resolution_result_record_id;",
    )?;
    Ok(())
}

/// Remove only schema-29 managed-resolution authority from an empty current
/// test store before it becomes a historical fixture. Never hide or discard a
/// reservation proof, a selected root, or an attempt binding.
#[cfg(test)]
fn drop_resolution_schema_29_for_legacy_fixture(
    connection: &rusqlite::Connection,
) -> rusqlite::Result<()> {
    if !fixture_table_exists(connection, "local_resolution_reservation_proofs")? {
        return Ok(());
    }
    let has_authority: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_resolution_reservation_proofs)
             OR EXISTS(SELECT 1 FROM local_resolution_reservation_roots)
             OR EXISTS(SELECT 1 FROM local_managed_resolution_attempt_bindings)",
        [],
        |row| row.get(0),
    )?;
    assert!(
        !has_authority,
        "cannot downgrade a fixture containing schema-29 managed-resolution authority"
    );
    connection.execute_batch(
        "DROP TRIGGER IF EXISTS local_managed_resolution_binding_no_delete;
         DROP TRIGGER IF EXISTS local_managed_resolution_binding_immutable;
         DROP TRIGGER IF EXISTS local_managed_resolution_binding_matches_attempt;
         DROP TRIGGER IF EXISTS local_resolution_reservation_root_no_delete;
         DROP TRIGGER IF EXISTS local_resolution_reservation_root_immutable;
         DROP TRIGGER IF EXISTS local_resolution_reservation_proof_no_delete;
         DROP TRIGGER IF EXISTS local_resolution_reservation_proof_immutable;
         DROP TRIGGER IF EXISTS local_resolution_reservation_proof_scope;
         DROP TABLE IF EXISTS local_managed_resolution_attempt_bindings;
         DROP TABLE IF EXISTS local_resolution_reservation_roots;
         DROP TABLE IF EXISTS local_resolution_reservation_proofs;",
    )?;
    Ok(())
}

/// Remove schema-29, schema-28 and schema-27 additions from a current test
/// store before a fixture lowers `schema_version` to exercise an older migration. Keep the original
/// resolution-attempt columns and rows: an old database has those, while it
/// cannot contain the later async-ledger bindings or wave archive references.
/// Refuse to discard any actual v27 or v28 data so this helper cannot make a test's
/// current-state records disappear under the guise of a historical fixture.
#[cfg(test)]
fn drop_resolution_schema_27_for_legacy_fixture(
    connection: &rusqlite::Connection,
) -> rusqlite::Result<()> {
    drop_resolution_schema_28_for_legacy_fixture(connection)?;
    let has_attempts = fixture_table_exists(connection, "resolution_attempts")?;
    let has_waves = fixture_table_exists(connection, "local_waves")?;
    if !has_attempts && !has_waves {
        return Ok(());
    }

    connection.execute_batch(
        "DROP TRIGGER IF EXISTS resolution_attempts_require_v27_charge;
         DROP TRIGGER IF EXISTS resolution_attempts_legacy_rows_immutable;
         DROP TRIGGER IF EXISTS resolution_attempts_never_delete;
         DROP TRIGGER IF EXISTS resolution_attempts_identity_immutable;
         DROP TRIGGER IF EXISTS resolution_attempts_lifecycle_guard;
         DROP TRIGGER IF EXISTS local_waves_resolution_request_pair_insert;
         DROP TRIGGER IF EXISTS local_waves_resolution_request_pair_update;
         DROP TRIGGER IF EXISTS local_waves_composition_source_pair_insert;
         DROP TRIGGER IF EXISTS local_waves_composition_source_pair_update;
         DROP INDEX IF EXISTS resolution_attempts_async_token;
         DROP INDEX IF EXISTS resolution_attempts_session_client_key;",
    )?;

    let attempt_columns = [
        "async_token",
        "client_key",
        "session_label",
        "attempt_number",
        "request_sha256",
        "input_sha256",
        "input_manifest_sha256",
        "dispatch_limits",
        "response_sha256",
        "result_record_id",
        "result_sha256",
        "detail",
        "async_state",
    ];
    if has_attempts && fixture_column_exists(connection, "resolution_attempts", "async_token")? {
        let has_v27_attempt_data: bool = connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM resolution_attempts
                 WHERE async_token IS NOT NULL
                    OR client_key IS NOT NULL
                    OR session_label IS NOT NULL
                    OR attempt_number IS NOT NULL
                    OR request_sha256 IS NOT NULL
                    OR input_sha256 IS NOT NULL
                    OR input_manifest_sha256 IS NOT NULL
                    OR dispatch_limits IS NOT NULL
                    OR response_sha256 IS NOT NULL
                    OR result_record_id IS NOT NULL
                    OR result_sha256 IS NOT NULL
                    OR detail IS NOT NULL
                    OR async_state != 'settled'
             )",
            [],
            |row| row.get(0),
        )?;
        assert!(
            !has_v27_attempt_data,
            "cannot downgrade a fixture containing schema-27 resolution-attempt data"
        );
        for column in attempt_columns {
            if fixture_column_exists(connection, "resolution_attempts", column)? {
                connection.execute_batch(&format!(
                    "ALTER TABLE resolution_attempts DROP COLUMN {column};"
                ))?;
            }
        }
    }

    let wave_columns = [
        "resolution_request_record_id",
        "resolution_request_object",
        "composition_source_record_id",
        "composition_source_object",
    ];
    if has_waves
        && fixture_column_exists(connection, "local_waves", "resolution_request_record_id")?
    {
        let has_v27_wave_data: bool = connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM local_waves
                 WHERE resolution_request_record_id IS NOT NULL
                    OR resolution_request_object IS NOT NULL
                    OR composition_source_record_id IS NOT NULL
                    OR composition_source_object IS NOT NULL
             )",
            [],
            |row| row.get(0),
        )?;
        assert!(
            !has_v27_wave_data,
            "cannot downgrade a fixture containing schema-27 wave references"
        );
        for column in wave_columns {
            if fixture_column_exists(connection, "local_waves", column)? {
                connection
                    .execute_batch(&format!("ALTER TABLE local_waves DROP COLUMN {column};"))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
fn fixture_table_exists(connection: &rusqlite::Connection, table: &str) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
         )",
        [table],
        |row| row.get(0),
    )
}

#[cfg(test)]
fn fixture_column_exists(
    connection: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> rusqlite::Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}
/// The file that marks a directory as a collaboration root.
pub const ROOT_MARKER: &str = ".aethyme-collaboration-root.json";
const ROOT_MARKER_KIND: &str = "aethyme-collaboration-root";
const STATE_DATABASE: &str = "state.db";

/// The directory name of one project under the collaboration root.
///
/// Opaque: lowercase ASCII letters, digits and `-`, at most 64 bytes. It will
/// be the enrolled ProjectId (#652; `proj:<base32>` stored as `proj-<base32>`)
/// and must never be the path-derived repository key, which changes when a
/// checkout moves.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectKey(String);

impl ProjectKey {
    pub fn parse(text: &str) -> Result<Self, CollaborationStateError> {
        let bytes = text.as_bytes();
        let valid = !bytes.is_empty()
            && bytes.len() <= 64
            && bytes[0] != b'-'
            && bytes
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
        if valid {
            Ok(Self(text.to_string()))
        } else {
            Err(CollaborationStateError::InvalidProjectKey {
                key: text.to_string(),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where the collaboration root came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "setting")]
pub enum RootSource {
    /// `<host state>/collaboration`, with the setting that chose the host
    /// state directory.
    HostState(HostStateSource),
    /// Named by the caller.
    Explicit,
}

/// The directory holding every project's collaboration state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollaborationRoot {
    path: PathBuf,
    source: RootSource,
}

impl CollaborationRoot {
    /// `<host state>/collaboration`.
    pub fn resolve() -> Result<Self, CollaborationStateError> {
        let (host_state, source) = crate::host_state::resolve_host_state_dir()
            .ok_or(CollaborationStateError::Unavailable)?;
        Ok(Self {
            path: host_state.join(ROOT_DIRECTORY),
            source: RootSource::HostState(source),
        })
    }

    /// The root under an explicitly named host state directory.
    pub fn under_host_state(host_state: &Path) -> Self {
        Self {
            path: host_state.join(ROOT_DIRECTORY),
            source: RootSource::Explicit,
        }
    }

    /// An exact collaboration-state root selected by an embedding broker.
    pub(crate) fn at_path(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            source: RootSource::Explicit,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn source(&self) -> RootSource {
        self.source
    }
}

/// A directory the collaboration root must stay disjoint from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ForbiddenRoot {
    pub path: PathBuf,
    pub kind: ForbiddenKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForbiddenKind {
    /// Holds session worktree roots; the orphan sweep and storage reclaim
    /// enumerate its children.
    WorktreeContainer,
    /// Swept by gate-cache GC and by the operating system.
    HostCache,
    /// The repository checkout, including its `.aethyme/` runtime files.
    RepositoryCheckout,
}

impl ForbiddenKind {
    fn describe(self) -> &'static str {
        match self {
            Self::WorktreeContainer => "worktree container",
            Self::HostCache => "host cache",
            Self::RepositoryCheckout => "repository checkout",
        }
    }

    fn remedy(self) -> &'static str {
        match self {
            Self::WorktreeContainer => {
                "move the worktree container (AETHYME_WORKTREE_ROOT or the repository's \
                 [worktrees] root) so it neither contains nor sits inside the host state directory"
            }
            Self::HostCache => {
                "point AETHYME_HOST_CACHE_DIR (or XDG_CACHE_HOME) away from the host state directory"
            }
            Self::RepositoryCheckout => {
                "point AETHYME_HOST_STATE_DIR (or XDG_STATE_HOME) outside the repository"
            }
        }
    }
}

/// Every directory the collaboration root of `main_root` must avoid: each
/// worktree container and host cache any process on this host could resolve
/// for this repository, and the checkout itself.
///
/// A container is listed for every host state directory a process could
/// resolve, not only this one, because a shell with different settings
/// sweeps its own (L0 slice C §4.1): a state directory named inside the
/// platform default's `worktrees/` would otherwise pass here and be
/// enumerated there.
pub fn forbidden_roots(main_root: &Path) -> Vec<ForbiddenRoot> {
    let absolute = |path: PathBuf| {
        if path.is_absolute() {
            path
        } else {
            main_root.join(path)
        }
    };
    let entry = |path: PathBuf, kind| ForbiddenRoot {
        path: absolute(path),
        kind,
    };
    let mut roots = vec![entry(
        main_root.to_path_buf(),
        ForbiddenKind::RepositoryCheckout,
    )];
    if let Some(container) =
        std::env::var_os("AETHYME_WORKTREE_ROOT").filter(|value| !value.is_empty())
    {
        roots.push(entry(container.into(), ForbiddenKind::WorktreeContainer));
    }
    if let Ok(Some(config)) = crate::worktree_location::WorktreeLocationConfig::load(main_root)
        && let Some(container) = config.root
    {
        roots.push(entry(container, ForbiddenKind::WorktreeContainer));
    }
    let (states, caches) = crate::host_state::host_directory_candidates();
    for state in states {
        roots.push(entry(
            state.join("worktrees"),
            ForbiddenKind::WorktreeContainer,
        ));
    }
    for cache in caches {
        roots.push(entry(cache, ForbiddenKind::HostCache));
    }
    roots
}

/// How a forbidden root overlaps the collaboration root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Overlap {
    Equals,
    Inside,
    Contains,
}

impl Overlap {
    fn describe(self) -> &'static str {
        match self {
            Self::Equals => "is",
            Self::Inside => "is inside",
            Self::Contains => "contains",
        }
    }
}

/// What a returned commit survives on this filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DurabilityProfile {
    /// The filesystem type as the kernel names it (`apfs`, `ext4`, ...).
    pub filesystem: String,
    /// False for network and FUSE mounts.
    pub local: bool,
    pub journal_mode: String,
    /// SQLite's `synchronous` level; 2 is `FULL`.
    pub synchronous: i64,
    /// Whether SQLite flushes with `F_FULLFSYNC` (macOS only).
    pub full_fsync: bool,
    /// True only when the filesystem and every setting match the supported
    /// profile documented in `local-v3-l2-state.md`.
    pub supported: bool,
    /// Why the profile is not supported, when it is not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limitation: Option<String>,
}

impl DurabilityProfile {
    /// The label a local receipt carries. An unsupported filesystem must not
    /// produce a receipt that reads the same as a supported one (§10.1).
    pub fn receipt_label(&self) -> &'static str {
        if self.supported {
            "local_durable"
        } else {
            "local_unverified"
        }
    }
}

/// One project's collaboration database, open.
#[derive(Debug)]
pub struct CollaborationStore {
    project: ProjectKey,
    project_dir: PathBuf,
    connection: Connection,
    durability: DurabilityProfile,
    schema_version: i64,
}

impl CollaborationStore {
    /// Open, creating if absent, `project`'s state under `root`, after
    /// checking `root` against `forbidden`.
    pub fn open(
        root: &CollaborationRoot,
        project: &ProjectKey,
        forbidden: &[ForbiddenRoot],
    ) -> Result<Self, CollaborationStateError> {
        let root_path = check_location(root.path(), forbidden)?;
        prepare_root(&root_path)?;
        let project_dir = root_path.join(project.as_str());
        ensure_private_directory(&project_dir)?;
        let database = project_dir.join(STATE_DATABASE);
        if !database.exists() {
            // Created private before SQLite opens it, so the database and the
            // WAL and shared-memory files SQLite derives from it are never
            // readable by others, even for a moment.
            create_private_file(&database)?;
            sync_directory(&project_dir)?;
        }
        let mut connection =
            Connection::open(&database).map_err(|source| sqlite(&database, source))?;
        let durability = configure(&connection, &project_dir).map_err(|e| sqlite(&database, e))?;
        let schema_version = initialise(&mut connection, project, &database)?;
        Ok(Self {
            project: project.clone(),
            project_dir,
            connection,
            durability,
            schema_version,
        })
    }

    pub fn project(&self) -> &ProjectKey {
        &self.project
    }

    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    pub fn durability(&self) -> &DurabilityProfile {
        &self.durability
    }

    pub fn schema_version(&self) -> i64 {
        self.schema_version
    }

    /// Pretend the store sits on another filesystem profile.
    #[cfg(test)]
    pub(crate) fn set_durability_for_test(&mut self, profile: DurabilityProfile) {
        self.durability = profile;
    }

    /// The database, read-only use.
    pub(crate) fn read_connection(&self) -> &Connection {
        &self.connection
    }

    /// The database, for the capture and archive slices built on this one.
    #[allow(dead_code)]
    pub(crate) fn connection(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

/// Where `project`'s state lives for `main_root`, after the same checks
/// [`open_for_repository`] makes, without creating or opening anything:
/// the root, the resolved project directory, and whether its database
/// exists yet. Inspection commands use it so that looking never writes.
pub fn locate_for_repository(
    main_root: &Path,
    project: &ProjectKey,
) -> Result<(CollaborationRoot, PathBuf, bool), CollaborationStateError> {
    if !crate::host_state::host_state_dir_is_explicit()
        && crate::host_state::path_is_ephemeral(main_root)
    {
        return Err(CollaborationStateError::EphemeralRepository {
            repository: main_root.to_path_buf(),
        });
    }
    let root = CollaborationRoot::resolve()?;
    let resolved = check_location(root.path(), &forbidden_roots(main_root))?;
    let project_dir = resolved.join(project.as_str());
    let exists = project_dir.join(STATE_DATABASE).is_file();
    Ok((root, project_dir, exists))
}

/// An existing project database opened read-only for inspection: no
/// pragmas, migrations, floor changes or created files.
#[derive(Debug)]
pub struct CollaborationSnapshot {
    pub project_dir: PathBuf,
    pub schema_version: i64,
    pub min_compatible_schema: i64,
    /// The stored schema is older than this binary's; the next writable
    /// open migrates it.
    pub needs_migration: bool,
    /// The profile writers open this database with: the filesystem, the
    /// stored journal mode, and the per-connection flush settings every
    /// writer applies (`synchronous=FULL`, `fullfsync`).
    pub durability: DurabilityProfile,
    connection: Connection,
}

impl CollaborationSnapshot {
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Whether the stored schema has `table` (an older store may not).
    pub(crate) fn has_table(&self, table: &str) -> bool {
        self.connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
    }
}

/// `project`'s database for `main_root`, opened read-only after the same
/// location and permission checks as [`open_for_repository`], or `None`
/// when it does not exist yet. Nothing is created, migrated or changed.
pub fn inspect_for_repository(
    main_root: &Path,
    project: &ProjectKey,
) -> Result<Option<CollaborationSnapshot>, CollaborationStateError> {
    let (_, project_dir, exists) = locate_for_repository(main_root, project)?;
    if !exists {
        return Ok(None);
    }
    inspect_at(project_dir, project).map(Some)
}

/// [`inspect_for_repository`] for a located project directory that holds a
/// database.
pub(crate) fn inspect_at(
    project_dir: PathBuf,
    project: &ProjectKey,
) -> Result<CollaborationSnapshot, CollaborationStateError> {
    if let Some(root) = project_dir.parent() {
        check_private(root)?;
    }
    check_private(&project_dir)?;
    let database = project_dir.join(STATE_DATABASE);
    let sqlite = |source| sqlite(&database, source);
    let connection = Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(2))
        .map_err(sqlite)?;
    let meta = |key: &str| -> Result<Option<String>, CollaborationStateError> {
        connection
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(_, Some(ref message))
                    if message.contains("no such table") =>
                {
                    CollaborationStateError::NotACollaborationDatabase {
                        path: database.clone(),
                    }
                }
                other => sqlite(other),
            })
    };
    let number = |key: &str| -> Result<i64, CollaborationStateError> {
        meta(key)?
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| CollaborationStateError::NotACollaborationDatabase {
                path: database.clone(),
            })
    };
    let schema_version = number("schema_version")?;
    let min_compatible_schema = number("min_compatible_schema")?;
    if min_compatible_schema > COLLABORATION_STATE_SCHEMA_VERSION {
        return Err(CollaborationStateError::SchemaTooNew {
            path: database,
            found: schema_version,
            min_compatible: min_compatible_schema,
        });
    }
    match meta("project_key")? {
        Some(owner) if owner == project.as_str() => {}
        Some(owner) => {
            return Err(CollaborationStateError::ProjectMismatch {
                path: database,
                expected: project.as_str().to_string(),
                found: owner,
            });
        }
        None => return Err(CollaborationStateError::NotACollaborationDatabase { path: database }),
    }
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(sqlite)?;
    let durability = assess(&project_dir, journal_mode, 2, cfg!(target_os = "macos"));
    Ok(CollaborationSnapshot {
        project_dir,
        schema_version,
        min_compatible_schema,
        needs_migration: schema_version < COLLABORATION_STATE_SCHEMA_VERSION,
        durability,
        connection,
    })
}

/// Open `project`'s state under `<host state>/collaboration`, checked
/// against every root [`forbidden_roots`] lists for `main_root`.
///
/// Like worktree placement, an implicit host state directory is withheld
/// from a repository under the system temporary directory, so a scratch
/// clone or test fixture never writes durable state into the real one.
pub fn open_for_repository(
    main_root: &Path,
    project: &ProjectKey,
) -> Result<CollaborationStore, CollaborationStateError> {
    if !crate::host_state::host_state_dir_is_explicit()
        && crate::host_state::path_is_ephemeral(main_root)
    {
        return Err(CollaborationStateError::EphemeralRepository {
            repository: main_root.to_path_buf(),
        });
    }
    CollaborationStore::open(
        &CollaborationRoot::resolve()?,
        project,
        &forbidden_roots(main_root),
    )
}

#[derive(Debug, thiserror::Error)]
pub enum CollaborationStateError {
    #[error(
        "no host state directory is available because HOME is unset; set AETHYME_HOST_STATE_DIR"
    )]
    Unavailable,
    #[error(
        "repository {} is under the system temporary directory, so the implicit host state \
         directory is withheld; set AETHYME_HOST_STATE_DIR explicitly",
        repository.display()
    )]
    EphemeralRepository { repository: PathBuf },
    #[error("collaboration root {} must be an absolute path", root.display())]
    RelativeRoot { root: PathBuf },
    #[error(
        "collaboration root {} {} the {} {}, which legacy cleanup can reach; {}",
        root.display(),
        overlap.describe(),
        kind.describe(),
        forbidden.display(),
        kind.remedy()
    )]
    Overlap {
        root: PathBuf,
        forbidden: PathBuf,
        kind: ForbiddenKind,
        overlap: Overlap,
    },
    #[error(
        "collaboration root {} is inside the Git working tree {}, where a clean or checkout \
         can remove it; point AETHYME_HOST_STATE_DIR (or XDG_STATE_HOME) outside it",
        root.display(),
        worktree.display()
    )]
    InsideGitWorktree { root: PathBuf, worktree: PathBuf },
    #[error(
        "{} must be owned by this user with no group or other access (found mode {:o}, owner \
         uid {}); fix it by hand with chmod 700, or remove it if it is not Aethyme's",
        path.display(),
        mode,
        owner
    )]
    InsecurePermissions {
        path: PathBuf,
        mode: u32,
        owner: u32,
    },
    #[error(
        "{} already holds files but has no {ROOT_MARKER}, so it is not a collaboration root; \
         move them away or choose another host state directory",
        root.display()
    )]
    ForeignRoot { root: PathBuf },
    #[error(
        "project key {key:?} is invalid: use 1-64 lowercase ASCII letters, digits and '-', \
         not starting with '-'"
    )]
    InvalidProjectKey { key: String },
    #[error(
        "{} has schema {found}, readable only by binaries supporting schema {min_compatible} or \
         later; this binary supports {COLLABORATION_STATE_SCHEMA_VERSION}. Upgrade Aethyme",
        path.display()
    )]
    SchemaTooNew {
        path: PathBuf,
        found: i64,
        min_compatible: i64,
    },
    #[error(
        "{} belongs to project {found:?}, not {expected:?}; a project directory is never \
         renamed or copied into place",
        path.display()
    )]
    ProjectMismatch {
        path: PathBuf,
        expected: String,
        found: String,
    },
    #[error("{} is not a collaboration state database", path.display())]
    NotACollaborationDatabase { path: PathBuf },
    #[error("{}", crate::host_state::describe_host_state_io(path, source))]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: {}", path.display(), crate::host_state::describe_host_state_sqlite(source))]
    Sqlite {
        path: PathBuf,
        source: rusqlite::Error,
    },
}

impl CollaborationStateError {
    /// A stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::EphemeralRepository { .. } => "ephemeral_repository",
            Self::RelativeRoot { .. } => "relative_root",
            Self::Overlap { .. } => "overlaps_cleanup_root",
            Self::InsideGitWorktree { .. } => "inside_git_worktree",
            Self::InsecurePermissions { .. } => "insecure_permissions",
            Self::ForeignRoot { .. } => "foreign_root",
            Self::InvalidProjectKey { .. } => "invalid_project_key",
            Self::SchemaTooNew { .. } => "schema_too_new",
            Self::ProjectMismatch { .. } => "project_mismatch",
            Self::NotACollaborationDatabase { .. } => "not_a_collaboration_database",
            Self::Io { .. } => "io",
            Self::Sqlite { .. } => "sqlite",
        }
    }
}

fn io(path: &Path, source: std::io::Error) -> CollaborationStateError {
    CollaborationStateError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn sqlite(path: &Path, source: rusqlite::Error) -> CollaborationStateError {
    CollaborationStateError::Sqlite {
        path: path.to_path_buf(),
        source,
    }
}

/// Resolve symlinks through the longest existing ancestor, so `/var` and
/// `/private/var`, or a symlinked container, compare as the same place.
fn resolve(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(resolved) = existing.canonicalize() {
            return rest
                .iter()
                .rev()
                .fold(resolved, |path: PathBuf, name| path.join(name));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The resolved root, or why it may not hold collaboration state.
fn check_location(
    root: &Path,
    forbidden: &[ForbiddenRoot],
) -> Result<PathBuf, CollaborationStateError> {
    if !root.is_absolute() {
        return Err(CollaborationStateError::RelativeRoot {
            root: root.to_path_buf(),
        });
    }
    let resolved = resolve(root);
    for entry in forbidden {
        let other = resolve(&entry.path);
        let overlap = if resolved == other {
            Overlap::Equals
        } else if resolved.starts_with(&other) {
            Overlap::Inside
        } else if other.starts_with(&resolved) {
            Overlap::Contains
        } else {
            continue;
        };
        return Err(CollaborationStateError::Overlap {
            root: resolved,
            forbidden: other,
            kind: entry.kind,
            overlap,
        });
    }
    if let Some(worktree) = resolved
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
    {
        return Err(CollaborationStateError::InsideGitWorktree {
            worktree: worktree.to_path_buf(),
            root: resolved,
        });
    }
    Ok(resolved)
}

/// Create the root with its marker, or accept an existing marked root.
fn prepare_root(root: &Path) -> Result<(), CollaborationStateError> {
    let marker = root.join(ROOT_MARKER);
    if marker.is_file() {
        return check_private(root);
    }
    if root.exists() {
        let mut entries = std::fs::read_dir(root).map_err(|source| io(root, source))?;
        if entries.next().is_some() {
            return Err(CollaborationStateError::ForeignRoot {
                root: root.to_path_buf(),
            });
        }
    }
    ensure_private_directory(root)?;
    let body = serde_json::json!({
        "schema_version": 1,
        "kind": ROOT_MARKER_KIND,
        "note": "Retained collaboration state: source, records and receipts. Not a worktree \
                 root or a cache. Aethyme cleanup never removes it.",
    });
    let bytes = serde_json::to_vec_pretty(&body).expect("static marker serializes");
    crate::atomic_file::with_synced_temporary(&marker, &bytes, |temporary| {
        std::fs::rename(temporary, &marker)
    })
    .map_err(|source| io(&marker, source))?;
    sync_directory(root)
}

/// Create `path` (and its parents) and require it to be private.
fn ensure_private_directory(path: &Path) -> Result<(), CollaborationStateError> {
    if !path.is_dir() {
        let parent = path.parent().unwrap_or(path);
        std::fs::create_dir_all(path).map_err(|source| io(path, source))?;
        crate::host_state::protect_host_state_path(path, true)
            .map_err(|source| io(path, source))?;
        sync_directory(parent)?;
    }
    check_private(path)
}

#[cfg(unix)]
fn check_private(path: &Path) -> Result<(), CollaborationStateError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path).map_err(|source| io(path, source))?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    let mode = metadata.mode() & 0o7777;
    if metadata.uid() != me || mode & 0o077 != 0 {
        return Err(CollaborationStateError::InsecurePermissions {
            path: path.to_path_buf(),
            mode,
            owner: metadata.uid(),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private(_path: &Path) -> Result<(), CollaborationStateError> {
    Ok(())
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> Result<(), CollaborationStateError> {
    use std::os::unix::fs::OpenOptionsExt;

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => Ok(()),
        // Another process created it first; its open applies the same mode.
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(io(path, source)),
    }
}

#[cfg(not(unix))]
fn create_private_file(_path: &Path) -> Result<(), CollaborationStateError> {
    Ok(())
}

/// Make a directory's entries durable. On macOS `sync_all` issues
/// `F_FULLFSYNC`, which also flushes the device cache.
pub(crate) fn sync_directory(path: &Path) -> Result<(), CollaborationStateError> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io(path, source))
}

fn configure(connection: &Connection, directory: &Path) -> rusqlite::Result<DurabilityProfile> {
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.pragma_update(None, "journal_mode", "wal")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "fullfsync", true)?;
    connection.pragma_update(None, "checkpoint_fullfsync", true)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    let journal_mode: String =
        connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    let synchronous: i64 = connection.pragma_query_value(None, "synchronous", |row| row.get(0))?;
    let full_fsync: bool = connection.pragma_query_value(None, "fullfsync", |row| row.get(0))?;
    Ok(assess(directory, journal_mode, synchronous, full_fsync))
}

/// The durability profile of a database in `directory` with these settings.
fn assess(
    directory: &Path,
    journal_mode: String,
    synchronous: i64,
    full_fsync: bool,
) -> DurabilityProfile {
    let (filesystem, local) = filesystem_of(directory);
    let mut limitations = Vec::new();
    if !local {
        limitations.push(format!(
            "{filesystem} is not a local filesystem, so flush and lock semantics are not known"
        ));
    } else if !SUPPORTED_FILESYSTEMS.contains(&filesystem.as_str()) {
        limitations.push(format!(
            "{filesystem} is not a supported filesystem ({})",
            SUPPORTED_FILESYSTEMS.join(", ")
        ));
    }
    if !journal_mode.eq_ignore_ascii_case("wal") {
        limitations.push(format!("journal mode is {journal_mode}, not wal"));
    }
    if synchronous != 2 {
        limitations.push(format!("synchronous is {synchronous}, not FULL (2)"));
    }
    if cfg!(target_os = "macos") && !full_fsync {
        limitations
            .push("fullfsync is off, so commits are not flushed past the drive cache".into());
    }
    DurabilityProfile {
        filesystem,
        local,
        journal_mode,
        synchronous,
        full_fsync,
        supported: limitations.is_empty(),
        limitation: (!limitations.is_empty()).then(|| limitations.join("; ")),
    }
}

/// Filesystems whose flush behaviour the supported profile assumes.
#[cfg(target_os = "macos")]
const SUPPORTED_FILESYSTEMS: &[&str] = &["apfs", "hfs"];
#[cfg(not(target_os = "macos"))]
const SUPPORTED_FILESYSTEMS: &[&str] = &["ext4", "xfs", "btrfs"];

/// The filesystem type holding `path`, and whether it is local.
#[cfg(target_os = "macos")]
fn filesystem_of(path: &Path) -> (String, bool) {
    use std::os::unix::ffi::OsStrExt;

    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return ("unknown".into(), false);
    };
    // SAFETY: statfs is a plain struct of integers and byte arrays, for which
    // all-zero is valid; the call only writes into it.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid NUL-terminated string and `stat` is writable.
    if unsafe { libc::statfs(path.as_ptr(), &mut stat) } != 0 {
        return ("unknown".into(), false);
    }
    // SAFETY: the kernel NUL-terminates f_fstypename within its 16 bytes.
    let name = unsafe { std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr()) };
    let local = stat.f_flags & libc::MNT_LOCAL as u32 != 0;
    (name.to_string_lossy().into_owned(), local)
}

#[cfg(target_os = "linux")]
fn filesystem_of(path: &Path) -> (String, bool) {
    use std::os::unix::ffi::OsStrExt;

    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return ("unknown".into(), false);
    };
    // SAFETY: statfs is a plain struct of integers, for which all-zero is
    // valid; the call only writes into it.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid NUL-terminated string and `stat` is writable.
    if unsafe { libc::statfs(path.as_ptr(), &mut stat) } != 0 {
        return ("unknown".into(), false);
    }
    #[allow(clippy::unnecessary_cast)]
    let magic = stat.f_type as i64;
    let (name, local) = match magic {
        0xEF53 => ("ext4", true),
        0x5846_5342 => ("xfs", true),
        0x9123_683E => ("btrfs", true),
        0x0102_1994 => ("tmpfs", true),
        0x6969 => ("nfs", false),
        0x517B | 0xFF53_4D42 | 0xFE53_4D42 => ("smb", false),
        0x6573_5546 => ("fuse", false),
        _ => return (format!("0x{magic:x}"), true),
    };
    (name.to_string(), local)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn filesystem_of(_path: &Path) -> (String, bool) {
    ("unknown".into(), false)
}

/// Create the schema on first open; on later opens check that this binary
/// may read it and that it belongs to `project`. Returns the schema version.
fn initialise(
    connection: &mut Connection,
    project: &ProjectKey,
    path: &Path,
) -> Result<i64, CollaborationStateError> {
    let sqlite = |source| sqlite(path, source);
    let has_meta: bool = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta'",
            [],
            |_| Ok(true),
        )
        .optional()
        .map_err(sqlite)?
        .unwrap_or(false);
    if !has_meta {
        let tables: i64 = connection
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
            .map_err(sqlite)?;
        if tables != 0 {
            return Err(CollaborationStateError::NotACollaborationDatabase {
                path: path.to_path_buf(),
            });
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite)?;
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS meta (
                     key TEXT PRIMARY KEY NOT NULL,
                     value TEXT NOT NULL
                 ) STRICT;",
            )
            .map_err(sqlite)?;
        for (key, value) in [
            // The version 1 layout; MIGRATIONS and the floor below bring it
            // up to date.
            ("schema_version", "1".to_string()),
            ("min_compatible_schema", "1".to_string()),
            ("project_key", project.as_str().to_string()),
        ] {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO meta (key, value) VALUES (?1, ?2)",
                    (key, value),
                )
                .map_err(sqlite)?;
        }
        transaction.commit().map_err(sqlite)?;
    }
    let meta = |key: &str| -> Result<Option<String>, CollaborationStateError> {
        connection
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(sqlite)
    };
    let number = |key: &str| -> Result<i64, CollaborationStateError> {
        meta(key)?
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| CollaborationStateError::NotACollaborationDatabase {
                path: path.to_path_buf(),
            })
    };
    let found = number("schema_version")?;
    let min_compatible = number("min_compatible_schema")?;
    if min_compatible > COLLABORATION_STATE_SCHEMA_VERSION {
        return Err(CollaborationStateError::SchemaTooNew {
            path: path.to_path_buf(),
            found,
            min_compatible,
        });
    }
    let owner =
        meta("project_key")?.ok_or_else(|| CollaborationStateError::NotACollaborationDatabase {
            path: path.to_path_buf(),
        })?;
    if owner != project.as_str() {
        return Err(CollaborationStateError::ProjectMismatch {
            path: path.to_path_buf(),
            expected: project.as_str().to_string(),
            found: owner,
        });
    }
    let mut found = found;
    for &(version, sql) in MIGRATIONS {
        if version <= found {
            continue;
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlite)?;
        // Another process may have migrated while this one waited for the lock.
        let current: i64 = transaction
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .map_err(sqlite)?;
        if current < version {
            transaction.execute_batch(sql).map_err(sqlite)?;
            transaction
                .execute(
                    "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                    [version.to_string()],
                )
                .map_err(sqlite)?;
        }
        transaction.commit().map_err(sqlite)?;
        found = found.max(version);
    }
    // Never lowered: a store another binary raised further keeps its floor.
    connection
        .execute(
            "UPDATE meta SET value = ?1
             WHERE key = 'min_compatible_schema' AND CAST(value AS INTEGER) < ?2",
            rusqlite::params![MIN_COMPATIBLE_SCHEMA.to_string(), MIN_COMPATIBLE_SCHEMA],
        )
        .map_err(sqlite)?;
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::os::unix::fs::PermissionsExt;

    fn key(text: &str) -> ProjectKey {
        ProjectKey::parse(text).unwrap()
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn open(
        host: &Path,
        forbidden: &[ForbiddenRoot],
    ) -> Result<CollaborationStore, CollaborationStateError> {
        CollaborationStore::open(
            &CollaborationRoot::under_host_state(host),
            &key("proj-a"),
            forbidden,
        )
    }

    fn forbid(path: &Path, kind: ForbiddenKind) -> ForbiddenRoot {
        ForbiddenRoot {
            path: path.to_path_buf(),
            kind,
        }
    }

    #[test]
    fn project_keys_are_opaque_directory_names() {
        for good in ["a", "proj-7k2m", "0", &"x".repeat(64)] {
            assert!(ProjectKey::parse(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-a",
            "A",
            "a/b",
            "..",
            "a.b",
            "proj:abc",
            &"x".repeat(65),
        ] {
            assert_eq!(
                ProjectKey::parse(bad).unwrap_err().code(),
                "invalid_project_key",
                "{bad}"
            );
        }
    }

    #[test]
    fn first_open_creates_a_private_marked_root_and_a_project_database() {
        let host = tempfile::tempdir().unwrap();
        let store = open(host.path(), &[]).unwrap();
        let root = host.path().join("collaboration");
        assert!(root.join(ROOT_MARKER).is_file());
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(store.project_dir()), 0o700);
        assert_eq!(mode(&store.project_dir().join("state.db")), 0o600);
        // SQLite gives its WAL the database's mode.
        assert_eq!(mode(&store.project_dir().join("state.db-wal")), 0o600);
        assert_eq!(store.schema_version(), COLLABORATION_STATE_SCHEMA_VERSION);

        let profile = store.durability();
        assert_eq!(profile.journal_mode, "wal");
        assert_eq!(profile.synchronous, 2);
        if cfg!(target_os = "macos") {
            assert!(profile.full_fsync);
        }
        assert_eq!(
            profile.supported,
            profile.limitation.is_none(),
            "{profile:?}"
        );
        assert_eq!(
            profile.receipt_label(),
            if profile.supported {
                "local_durable"
            } else {
                "local_unverified"
            }
        );

        // Schema DDL may instantiate SQLite's empty transient temp database.
        // There is exactly one persistent database and no attached shared DB.
        let databases: Vec<(String, String)> = store
            .connection
            .prepare("SELECT name,file FROM pragma_database_list ORDER BY seq")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let persistent: Vec<_> = databases
            .iter()
            .filter(|(name, _)| name != "temp")
            .collect();
        assert_eq!(
            persistent,
            [&(
                "main".into(),
                store
                    .project_dir()
                    .join("state.db")
                    .canonicalize()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned()
            )]
        );
        assert!(
            databases
                .iter()
                .all(|(name, file)| name != "temp" || file.is_empty())
        );
    }

    #[test]
    fn reopening_is_idempotent() {
        let host = tempfile::tempdir().unwrap();
        drop(open(host.path(), &[]).unwrap());
        let marker = std::fs::read(host.path().join("collaboration").join(ROOT_MARKER)).unwrap();
        let again = open(host.path(), &[]).unwrap();
        assert_eq!(again.schema_version(), COLLABORATION_STATE_SCHEMA_VERSION);
        assert_eq!(
            std::fs::read(host.path().join("collaboration").join(ROOT_MARKER)).unwrap(),
            marker
        );
    }

    /// The location rule is about where legacy cleanup can reach, so it is
    /// checked in every direction and through symlinks.
    #[test]
    fn a_root_overlapping_a_cleanup_root_is_refused() {
        let host = tempfile::tempdir().unwrap();
        let root = host.path().join("collaboration");
        let elsewhere = tempfile::tempdir().unwrap();
        let link = elsewhere.path().join("link-to-host");
        std::os::unix::fs::symlink(host.path(), &link).unwrap();
        let cases = [
            (
                forbid(&root, ForbiddenKind::WorktreeContainer),
                Overlap::Equals,
            ),
            (
                forbid(host.path(), ForbiddenKind::WorktreeContainer),
                Overlap::Inside,
            ),
            (forbid(&link, ForbiddenKind::HostCache), Overlap::Inside),
            (
                forbid(host.path(), ForbiddenKind::RepositoryCheckout),
                Overlap::Inside,
            ),
            (
                forbid(&root.join("proj-a/x"), ForbiddenKind::WorktreeContainer),
                Overlap::Contains,
            ),
        ];
        for (forbidden, expected) in cases {
            let error = open(host.path(), std::slice::from_ref(&forbidden)).unwrap_err();
            match &error {
                CollaborationStateError::Overlap { overlap, kind, .. } => {
                    assert_eq!((*overlap, *kind), (expected, forbidden.kind), "{error}");
                }
                other => panic!("{forbidden:?}: {other}"),
            }
            assert!(
                !root.exists(),
                "a refusal must create nothing: {forbidden:?}"
            );
        }
        // A sibling is not an overlap.
        open(
            host.path(),
            &[forbid(
                &host.path().join("worktrees"),
                ForbiddenKind::WorktreeContainer,
            )],
        )
        .unwrap();
    }

    #[test]
    fn a_root_inside_a_git_working_tree_is_refused() {
        let host = tempfile::tempdir().unwrap();
        std::fs::create_dir(host.path().join(".git")).unwrap();
        let error = open(&host.path().join("nested"), &[]).unwrap_err();
        assert_eq!(error.code(), "inside_git_worktree", "{error}");
    }

    #[test]
    fn loose_permissions_are_refused_not_repaired() {
        let host = tempfile::tempdir().unwrap();
        drop(open(host.path(), &[]).unwrap());
        let root = host.path().join("collaboration");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = open(host.path(), &[]).unwrap_err();
        assert_eq!(error.code(), "insecure_permissions", "{error}");
        assert_eq!(mode(&root), 0o755);

        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let project = root.join("proj-a");
        std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert_eq!(
            open(host.path(), &[]).unwrap_err().code(),
            "insecure_permissions"
        );
    }

    #[test]
    fn an_unmarked_non_empty_root_is_foreign_and_left_alone() {
        let host = tempfile::tempdir().unwrap();
        let root = host.path().join("collaboration");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("someone-else.txt"), "keep\n").unwrap();
        assert_eq!(open(host.path(), &[]).unwrap_err().code(), "foreign_root");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    }

    #[test]
    fn a_database_from_elsewhere_is_refused() {
        let host = tempfile::tempdir().unwrap();
        drop(open(host.path(), &[]).unwrap());
        let root = host.path().join("collaboration");

        // Copied into another project's place.
        std::fs::rename(root.join("proj-a"), root.join("proj-b")).unwrap();
        let error = CollaborationStore::open(
            &CollaborationRoot::under_host_state(host.path()),
            &key("proj-b"),
            &[],
        )
        .unwrap_err();
        assert_eq!(error.code(), "project_mismatch", "{error}");

        // Some other SQLite file.
        let project = root.join("proj-c");
        std::fs::create_dir(&project).unwrap();
        std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o700)).unwrap();
        Connection::open(project.join("state.db"))
            .unwrap()
            .execute_batch("CREATE TABLE unrelated (x)")
            .unwrap();
        let error = CollaborationStore::open(
            &CollaborationRoot::under_host_state(host.path()),
            &key("proj-c"),
            &[],
        )
        .unwrap_err();
        assert_eq!(error.code(), "not_a_collaboration_database", "{error}");
    }

    /// A newer database stays readable until a release raises the floor; it
    /// never locks every older binary out the way an exact-version check does.
    #[test]
    fn a_newer_schema_opens_until_its_floor_passes_this_binary() {
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path(), &[]).unwrap();
        store
            .connection()
            .execute(
                "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                [(COLLABORATION_STATE_SCHEMA_VERSION + 1).to_string()],
            )
            .unwrap();
        drop(store);
        assert_eq!(
            open(host.path(), &[]).unwrap().schema_version(),
            COLLABORATION_STATE_SCHEMA_VERSION + 1
        );

        let mut store = open(host.path(), &[]).unwrap();
        store
            .connection()
            .execute(
                "UPDATE meta SET value = ?1 WHERE key = 'min_compatible_schema'",
                [(COLLABORATION_STATE_SCHEMA_VERSION + 1).to_string()],
            )
            .unwrap();
        drop(store);
        let error = open(host.path(), &[]).unwrap_err();
        assert_eq!(error.code(), "schema_too_new", "{error}");
    }

    /// Schema 4 raises the floor, for new stores and for stores migrated from
    /// schema 3, so a binary that captures without the archive lock can no
    /// longer open a store that reclamation manages.
    #[test]
    fn the_floor_is_raised_for_new_and_migrated_stores() {
        let floor = |store: &CollaborationStore| -> i64 {
            store
                .read_connection()
                .query_row(
                    "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'min_compatible_schema'",
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        };
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path(), &[]).unwrap();
        assert_eq!(floor(&store), MIN_COMPATIBLE_SCHEMA);
        // As a schema 3 binary left it.
        drop_resolution_schema_27_for_legacy_fixture(store.connection()).unwrap();
        crate::collaboration_sensitive::drop_sensitive_response_tables_for_legacy_fixture(
            store.connection(),
        )
        .unwrap();
        store
            .connection()
            .execute_batch(
                "DROP TABLE IF EXISTS reclaimed_contribution_revisions; DROP TABLE IF EXISTS contribution_revision_identity_bases; DROP TABLE IF EXISTS contribution_revision_heads; DROP TABLE IF EXISTS contribution_revisions; DROP TABLE IF EXISTS local_logical_contributions; DROP TABLE IF EXISTS local_project_file_ids; DROP TABLE IF EXISTS local_project_bindings; DROP INDEX IF EXISTS capture_receipts_operation_lineage; DROP TABLE IF EXISTS contribution_briefs; DROP TABLE IF EXISTS local_capture_notice_outcomes; DROP TABLE IF EXISTS local_capture_notices; DROP TABLE local_native_gate_roots; DROP TABLE local_native_gate_jobs; DROP TABLE local_import_rehearsals; DROP TABLE local_imported_records; DROP TABLE local_imported_sources; DROP TABLE local_imports; DROP TABLE local_adoption_receipts; DROP TABLE local_agent_dispatches; DROP TABLE local_agent_reconciliations; DROP TABLE local_handoffs; DROP TABLE local_agent_sessions; DROP TABLE local_exchange_messages; DROP TABLE local_exchange_deliveries; DROP TABLE local_exchange_streams; DROP TABLE local_exchange_events; DROP TABLE local_exchange_projection; DROP TABLE local_exchange_observations; DROP TABLE local_exchange_bindings; DROP TRIGGER outbox_capture_insert; DROP TRIGGER outbox_capture_update; DROP TRIGGER outbox_capture_delete; DROP TABLE local_generation_roots; DROP TABLE local_generation_receipts; DROP TABLE local_generations; DROP TABLE local_acceptance_qualifications; DROP TABLE local_acceptance_receipts; DROP TABLE local_acceptance_bindings; DROP TABLE local_source_transport_receipts; DROP TABLE local_source_transports; DROP TABLE local_verification_evidence; DROP TABLE local_verifier_producers; DROP TABLE local_authority_state; DROP TABLE local_wave_arrivals;
                 DROP TABLE local_waves;
                 UPDATE meta SET value = '3' WHERE key = 'schema_version';
                 UPDATE meta SET value = '1' WHERE key = 'min_compatible_schema';",
            )
            .unwrap();
        drop(store);
        let store = open(host.path(), &[]).unwrap();
        assert_eq!(store.schema_version(), COLLABORATION_STATE_SCHEMA_VERSION);
        assert_eq!(floor(&store), MIN_COMPATIBLE_SCHEMA);
    }

    #[test]
    fn schema_twenty_three_migrates_capture_rows_without_rewriting_them() {
        use crate::collaboration_cli::project_test_support::BoundProjectFixture;

        let mut fixture = BoundProjectFixture::new().expect("real bound Local fixture");
        let base = fixture.bootstrap_commit.clone();
        let result = fixture
            .commit_file(
                "migration-sentinel.txt",
                b"capture survives schema 23 migration\n",
            )
            .expect("real committed result");
        fixture
            .capture_operation("op-schema23-preserved", &base, &result)
            .expect("real retained capture");
        let project = fixture.store.project().clone();
        let host_path = fixture.host.path().to_path_buf();
        let project_dir = fixture.store.project_dir().to_path_buf();
        let before: (String, String, String, String, String, i64) = fixture
            .store
            .read_connection()
            .query_row(
                "SELECT receipt_record_id,receipt_sha256,lineage_record_id,
                        base_snapshot,result_snapshot,committed_ms
                 FROM capture_receipts WHERE operation_id='op-schema23-preserved'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        drop(fixture.store);

        let connection = Connection::open(project_dir.join(STATE_DATABASE)).unwrap();
        drop_identity_revision_schema_for_legacy_fixture(&connection).unwrap();
        crate::collaboration_sensitive::drop_sensitive_response_tables_for_legacy_fixture(
            &connection,
        )
        .unwrap();
        connection
            .execute_batch(
                "DROP TRIGGER IF EXISTS local_capture_notice_outcome_monotone;
                 DROP TABLE IF EXISTS local_capture_notice_outcomes;
                 DROP TABLE IF EXISTS local_capture_notices;
                 UPDATE meta SET value='23'
                     WHERE key IN ('schema_version','min_compatible_schema');",
            )
            .unwrap();
        drop(connection);

        let migrated = CollaborationStore::open(
            &CollaborationRoot::under_host_state(&host_path),
            &project,
            &[],
        )
        .unwrap();
        assert_eq!(
            migrated.schema_version(),
            COLLABORATION_STATE_SCHEMA_VERSION
        );
        let floor: i64 = migrated
            .read_connection()
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key='min_compatible_schema'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(floor, MIN_COMPATIBLE_SCHEMA);
        let after: (String, String, String, String, String, i64) = migrated
            .read_connection()
            .query_row(
                "SELECT receipt_record_id,receipt_sha256,lineage_record_id,
                        base_snapshot,result_snapshot,committed_ms
                 FROM capture_receipts WHERE operation_id='op-schema23-preserved'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            after, before,
            "schema migration rewrote retained receipt authority"
        );
        let outbox_count: i64 = migrated
            .read_connection()
            .query_row(
                "SELECT count(*) FROM outbox WHERE operation_id='op-schema23-preserved'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(outbox_count, 1);
    }

    /// Migrations run in list order and skip versions at or below the
    /// stored one, so a gap or a reordering would leave some store without
    /// a step. They must be 2, 3, ... up to this binary's version.
    #[test]
    fn migrations_are_contiguous_and_end_at_this_binarys_version() {
        let versions: Vec<i64> = MIGRATIONS.iter().map(|(version, _)| *version).collect();
        let expected: Vec<i64> = (2..=COLLABORATION_STATE_SCHEMA_VERSION).collect();
        assert_eq!(versions, expected);
        assert_eq!(MIN_COMPATIBLE_SCHEMA, 29);
        const { assert!(MIN_COMPATIBLE_SCHEMA <= COLLABORATION_STATE_SCHEMA_VERSION) };
        // Earlier writers cannot preserve immutable revision heads, durable
        // asynchronous attempts or the archive roots those attempts name.
        const { assert!(MIN_COMPATIBLE_SCHEMA >= 29) };
    }

    #[test]
    fn schema_29_preserves_shared_charges_and_binds_distinct_attempt_axes() {
        #[derive(Debug, PartialEq, Eq)]
        struct ProofSnapshot {
            reservation: String,
            target: String,
            issuer: String,
            authority_epoch: String,
            client_key: String,
            wave: String,
            expected_version: i64,
            request_record_id: String,
            request_object_sha256: String,
            composition_source_record_id: String,
            composition_source_object_sha256: String,
            input_manifest_object_sha256: String,
            trigger_sha256: String,
            project_policy: String,
            objective_policy: String,
            price_profile: String,
            dispatch_limits: String,
            reserved_envelope: Vec<u8>,
            reservation_attempt_limit: i64,
            deadline_ms: i64,
            created_ms: i64,
        }
        #[derive(Debug, PartialEq, Eq)]
        struct BindingSnapshot {
            binding_id: String,
            reservation: String,
            attempt_id: i64,
            authority_epoch: String,
            reservation_version: i64,
            shared_attempt_number: i64,
            reservation_attempt_number: i64,
            route: String,
            created_ms: i64,
        }
        #[derive(Debug, PartialEq, Eq)]
        struct ReservationSnapshot {
            target: String,
            revision: String,
            wave: String,
            phase: String,
            version: i64,
            attempts: i64,
            spent: Vec<u8>,
            receipt: Option<String>,
            detail: Option<String>,
            observed_overrun: Option<Vec<u8>>,
        }
        #[derive(Debug, PartialEq, Eq)]
        struct AttemptSnapshot {
            attempt_id: i64,
            decision_key: String,
            kind: String,
            members: String,
            paths: String,
            request_record_id: String,
            resolver: String,
            outcome: Option<String>,
            charged_ms: i64,
            async_token: Option<String>,
            client_key: Option<String>,
            session_label: Option<String>,
            attempt_number: Option<i64>,
            request_sha256: Option<String>,
            input_sha256: Option<String>,
            input_manifest_sha256: Option<String>,
            dispatch_limits: Option<String>,
            response_sha256: Option<String>,
            result_record_id: Option<String>,
            result_sha256: Option<String>,
            detail: Option<String>,
            async_state: String,
        }
        #[derive(Debug, PartialEq, Eq)]
        struct BindingAuthoritySnapshot {
            proof: ProofSnapshot,
            wave_target_revision: (String, String),
            roots: Vec<String>,
            bindings: Vec<BindingSnapshot>,
            reservation: ReservationSnapshot,
            attempts: Vec<AttemptSnapshot>,
        }

        fn snapshot(
            connection: &rusqlite::Connection,
            reservation: &str,
        ) -> rusqlite::Result<BindingAuthoritySnapshot> {
            let proof = connection.query_row(
                "SELECT reservation,target,issuer,authority_epoch,client_key,wave,expected_version,
                        request_record_id,request_object_sha256,
                        composition_source_record_id,composition_source_object_sha256,
                        input_manifest_object_sha256,trigger_sha256,project_policy,
                        objective_policy,price_profile,dispatch_limits,reserved_envelope,
                        reservation_attempt_limit,deadline_ms,created_ms
                 FROM local_resolution_reservation_proofs WHERE reservation=?1",
                [reservation],
                |row| {
                    Ok(ProofSnapshot {
                        reservation: row.get(0)?,
                        target: row.get(1)?,
                        issuer: row.get(2)?,
                        authority_epoch: row.get(3)?,
                        client_key: row.get(4)?,
                        wave: row.get(5)?,
                        expected_version: row.get(6)?,
                        request_record_id: row.get(7)?,
                        request_object_sha256: row.get(8)?,
                        composition_source_record_id: row.get(9)?,
                        composition_source_object_sha256: row.get(10)?,
                        input_manifest_object_sha256: row.get(11)?,
                        trigger_sha256: row.get(12)?,
                        project_policy: row.get(13)?,
                        objective_policy: row.get(14)?,
                        price_profile: row.get(15)?,
                        dispatch_limits: row.get(16)?,
                        reserved_envelope: row.get(17)?,
                        reservation_attempt_limit: row.get(18)?,
                        deadline_ms: row.get(19)?,
                        created_ms: row.get(20)?,
                    })
                },
            )?;
            let mut root_statement = connection.prepare(
                "SELECT lineage_record_id FROM local_resolution_reservation_roots
                 WHERE reservation=?1 ORDER BY lineage_record_id",
            )?;
            let roots: Vec<String> = root_statement
                .query_map([reservation], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let wave_target_revision = connection.query_row(
                "SELECT target,revision FROM local_waves WHERE wave=?1",
                [proof.wave.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let mut binding_statement = connection.prepare(
                "SELECT binding_id,reservation,attempt_id,authority_epoch,
                        reservation_version,shared_attempt_number,
                        reservation_attempt_number,route,created_ms
                 FROM local_managed_resolution_attempt_bindings
                 WHERE reservation=?1 ORDER BY binding_id",
            )?;
            let bindings = binding_statement
                .query_map([reservation], |row| {
                    Ok(BindingSnapshot {
                        binding_id: row.get(0)?,
                        reservation: row.get(1)?,
                        attempt_id: row.get(2)?,
                        authority_epoch: row.get(3)?,
                        reservation_version: row.get(4)?,
                        shared_attempt_number: row.get(5)?,
                        reservation_attempt_number: row.get(6)?,
                        route: row.get(7)?,
                        created_ms: row.get(8)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let reservation_state = connection.query_row(
                "SELECT target,revision,wave,phase,version,attempts,spent,receipt,detail,
                        observed_overrun
                 FROM local_budget_reservations
                 WHERE reservation=?1",
                [reservation],
                |row| {
                    Ok(ReservationSnapshot {
                        target: row.get(0)?,
                        revision: row.get(1)?,
                        wave: row.get(2)?,
                        phase: row.get(3)?,
                        version: row.get(4)?,
                        attempts: row.get(5)?,
                        spent: row.get(6)?,
                        receipt: row.get(7)?,
                        detail: row.get(8)?,
                        observed_overrun: row.get(9)?,
                    })
                },
            )?;
            let mut attempt_statement = connection.prepare(
                "SELECT attempt_id,decision_key,kind,members,paths,request_record_id,resolver,
                        outcome,charged_ms,async_token,client_key,session_label,attempt_number,
                        request_sha256,input_sha256,input_manifest_sha256,dispatch_limits,
                        response_sha256,result_record_id,result_sha256,detail,async_state
                 FROM resolution_attempts WHERE client_key=?1 ORDER BY attempt_id",
            )?;
            let attempts = attempt_statement
                .query_map([proof.client_key.as_str()], |row| {
                    Ok(AttemptSnapshot {
                        attempt_id: row.get(0)?,
                        decision_key: row.get(1)?,
                        kind: row.get(2)?,
                        members: row.get(3)?,
                        paths: row.get(4)?,
                        request_record_id: row.get(5)?,
                        resolver: row.get(6)?,
                        outcome: row.get(7)?,
                        charged_ms: row.get(8)?,
                        async_token: row.get(9)?,
                        client_key: row.get(10)?,
                        session_label: row.get(11)?,
                        attempt_number: row.get(12)?,
                        request_sha256: row.get(13)?,
                        input_sha256: row.get(14)?,
                        input_manifest_sha256: row.get(15)?,
                        dispatch_limits: row.get(16)?,
                        response_sha256: row.get(17)?,
                        result_record_id: row.get(18)?,
                        result_sha256: row.get(19)?,
                        detail: row.get(20)?,
                        async_state: row.get(21)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(BindingAuthoritySnapshot {
                proof,
                wave_target_revision,
                roots,
                bindings,
                reservation: reservation_state,
                attempts,
            })
        }

        fn insert_binding(
            connection: &rusqlite::Connection,
            binding_id: &str,
            reservation: &str,
            attempt_id: i64,
            shared_attempt_number: i64,
            reservation_attempt_number: i64,
        ) -> rusqlite::Result<usize> {
            connection.execute(
                "INSERT INTO local_managed_resolution_attempt_bindings
                     (binding_id,reservation,attempt_id,authority_epoch,
                      reservation_version,shared_attempt_number,
                      reservation_attempt_number,route,created_ms)
                 VALUES (?1,?2,?3,'epoch-1',1,?4,?5,
                         'local_managed_resolution_v1',101)",
                rusqlite::params![
                    binding_id,
                    reservation,
                    attempt_id,
                    shared_attempt_number,
                    reservation_attempt_number,
                ],
            )
        }

        // Fixtures vary each persisted route and attempt binding independently.
        #[allow(clippy::too_many_arguments)]
        fn insert_attempt_with_route(
            connection: &rusqlite::Connection,
            request_record_id: &str,
            request_sha: &str,
            manifest_sha: &str,
            client_key: &str,
            resolver: &str,
            session_label: Option<&str>,
            shared_attempt_number: i64,
        ) -> rusqlite::Result<i64> {
            connection.execute(
                "INSERT INTO resolution_attempts
                     (decision_key,kind,members,paths,request_record_id,resolver,
                      charged_ms,attempt_number,request_sha256,input_manifest_sha256,
                      client_key,session_label,async_state)
                 VALUES ('decision:shared','failed_checks','lineage-a','',?1,
                         ?6,456,?7,?2,?3,?4,?5,'dispatching')",
                rusqlite::params![
                    request_record_id,
                    request_sha,
                    manifest_sha,
                    client_key,
                    session_label,
                    resolver,
                    shared_attempt_number
                ],
            )?;
            Ok(connection.last_insert_rowid())
        }

        fn insert_attempt(
            connection: &rusqlite::Connection,
            request_record_id: &str,
            request_sha: &str,
            manifest_sha: &str,
            client_key: &str,
            shared_attempt_number: i64,
        ) -> rusqlite::Result<i64> {
            insert_attempt_with_route(
                connection,
                request_record_id,
                request_sha,
                manifest_sha,
                client_key,
                "local-managed-resolution",
                None,
                shared_attempt_number,
            )
        }

        fn assert_rejected_without_mutation(
            connection: &rusqlite::Connection,
            reservation: &str,
            description: &str,
            insert: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<usize>,
        ) {
            let before = snapshot(connection, reservation).unwrap();
            assert!(
                insert(connection).is_err(),
                "{description} must be rejected"
            );
            let after = snapshot(connection, reservation).unwrap();
            assert_eq!(
                after, before,
                "{description} must not mutate proof, roots, bindings, or reservation state"
            );
        }

        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path(), &[]).unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO resolution_attempts
                     (decision_key,kind,members,paths,request_record_id,resolver,
                      charged_ms,attempt_number,async_state)
                 VALUES ('decision:shared','failed_checks','lineage-a','',
                         'request:legacy','manual',123,1,'dispatching')",
                [],
            )
            .unwrap();
        drop_resolution_schema_29_for_legacy_fixture(store.connection()).unwrap();
        store
            .connection()
            .execute_batch(
                "UPDATE meta SET value='28'
                     WHERE key IN ('schema_version','min_compatible_schema');",
            )
            .unwrap();
        drop(store);

        let mut migrated = open(host.path(), &[]).unwrap();
        let legacy: (String, String, i64, Option<String>) = migrated
            .read_connection()
            .query_row(
                "SELECT request_record_id,async_state,attempt_number,async_token
                 FROM resolution_attempts WHERE decision_key='decision:shared'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            legacy,
            ("request:legacy".into(), "dispatching".into(), 1, None)
        );

        let request_sha = "a".repeat(64);
        let source_sha = "f".repeat(64);
        let manifest_sha = "b".repeat(64);
        let trigger_sha = "c".repeat(64);
        migrated
            .connection()
            .execute_batch(
                "INSERT INTO local_targets (target,issuer) VALUES ('target','issuer');
                 INSERT INTO local_target_revisions (revision,target,body)
                     VALUES ('revision','target',X'01');
                 INSERT INTO local_target_revisions (revision,target,body)
                     VALUES ('revision:other','target',X'11');
                 INSERT INTO local_targets (target,issuer)
                     VALUES ('target:other','issuer');
                 UPDATE local_targets SET active_revision='revision'
                     WHERE target='target';",
            )
            .unwrap();
        migrated
            .connection()
            .execute(
                "INSERT INTO local_waves
                     (wave,target,revision,sequence,body,phase,
                      resolution_request_record_id,resolution_request_object,
                      composition_source_record_id,composition_source_object)
                 VALUES ('wave','target','revision',1,X'02','verifying',?1,?2,?3,?4)",
                rusqlite::params!["request:managed", request_sha, "source:managed", source_sha],
            )
            .unwrap();
        migrated
            .connection()
            .execute_batch(
                "INSERT INTO local_waves
                     (wave,target,revision,sequence,body,phase)
                 VALUES ('wave:wrong-target','target:other','revision',4,X'12','completed');
                 INSERT INTO local_waves
                     (wave,target,revision,sequence,body,phase)
                 VALUES ('wave:wrong-revision','target','revision:other',2,X'13','completed');",
            )
            .unwrap();
        migrated
            .connection()
            .execute_batch(
                "INSERT INTO local_budget_reservations
                     (reservation,nonce,target,revision,wave,root,parent,body,
                      phase,spent,attempts,version)
                 VALUES ('reservation:wrong-wave-target','nonce:wrong-wave-target',
                         'target','revision','wave:wrong-target',
                         'reservation:wrong-wave-target',NULL,X'14',
                         'completion_pending',X'00',1,1);
                 INSERT INTO local_budget_reservations
                     (reservation,nonce,target,revision,wave,root,parent,body,
                      phase,spent,attempts,version)
                 VALUES ('reservation:wrong-wave-revision','nonce:wrong-wave-revision',
                         'target','revision','wave:wrong-revision',
                         'reservation:wrong-wave-revision',NULL,X'15',
                         'completion_pending',X'00',1,1);",
            )
            .unwrap();
        let wrong_wave_target_proof = migrated.connection().execute(
            "INSERT INTO local_resolution_reservation_proofs
                 (reservation,target,issuer,authority_epoch,client_key,wave,expected_version,
                  request_record_id,request_object_sha256,composition_source_record_id,
                  composition_source_object_sha256,input_manifest_object_sha256,
                  trigger_sha256,project_policy,objective_policy,price_profile,
                  dispatch_limits,reserved_envelope,reservation_attempt_limit,
                  deadline_ms,created_ms)
             VALUES ('reservation:wrong-wave-target','target','issuer','epoch-1',
                     'client-key:wrong-wave-target','wave:wrong-target',0,
                     'request:managed',?1,'source:managed',?2,?3,?4,
                     'project-policy','objective-policy','price-profile',
                     'v1:1024:10:1024:4096:1024',X'16',1,999,100)",
            rusqlite::params![request_sha, source_sha, manifest_sha, trigger_sha],
        );
        assert!(
            wrong_wave_target_proof.is_err(),
            "a proof cannot bind a reservation to a wave for another target"
        );
        let wrong_wave_target_proof_count: i64 = migrated
            .read_connection()
            .query_row(
                "SELECT count(*) FROM local_resolution_reservation_proofs
                 WHERE reservation='reservation:wrong-wave-target'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(wrong_wave_target_proof_count, 0);
        let wrong_wave_revision_proof = migrated.connection().execute(
            "INSERT INTO local_resolution_reservation_proofs
                 (reservation,target,issuer,authority_epoch,client_key,wave,expected_version,
                  request_record_id,request_object_sha256,composition_source_record_id,
                  composition_source_object_sha256,input_manifest_object_sha256,
                  trigger_sha256,project_policy,objective_policy,price_profile,
                  dispatch_limits,reserved_envelope,reservation_attempt_limit,
                  deadline_ms,created_ms)
             VALUES ('reservation:wrong-wave-revision','target','issuer','epoch-1',
                     'client-key:wrong-wave-revision','wave:wrong-revision',0,
                     'request:managed',?1,'source:managed',?2,?3,?4,
                     'project-policy','objective-policy','price-profile',
                     'v1:1024:10:1024:4096:1024',X'17',1,999,100)",
            rusqlite::params![request_sha, source_sha, manifest_sha, trigger_sha],
        );
        assert!(
            wrong_wave_revision_proof.is_err(),
            "a proof cannot bind a reservation to a different wave revision"
        );
        let wrong_wave_revision_proof_count: i64 = migrated
            .read_connection()
            .query_row(
                "SELECT count(*) FROM local_resolution_reservation_proofs
                 WHERE reservation='reservation:wrong-wave-revision'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(wrong_wave_revision_proof_count, 0);
        migrated
            .connection()
            .execute(
                "INSERT INTO local_budget_reservations
                     (reservation,nonce,target,revision,wave,root,parent,body,
                      phase,spent,attempts,version)
                 VALUES ('reservation','nonce','target','revision','wave',
                         'reservation',NULL,X'03','completion_pending',X'00',1,1)",
                [],
            )
            .unwrap();
        migrated
            .connection()
            .execute(
                "INSERT INTO local_budget_reservations
                     (reservation,nonce,target,revision,wave,root,parent,body,
                      phase,spent,attempts,version)
                 VALUES ('reservation:stale-version','nonce:stale-version','target',
                         'revision','wave','reservation:stale-version',NULL,X'0d',
                         'reserved',X'00',0,0)",
                [],
            )
            .unwrap();
        let stale_version_proof = migrated.connection().execute(
            "INSERT INTO local_resolution_reservation_proofs
                 (reservation,target,issuer,authority_epoch,client_key,wave,expected_version,
                  request_record_id,request_object_sha256,composition_source_record_id,
                  composition_source_object_sha256,input_manifest_object_sha256,
                  trigger_sha256,project_policy,objective_policy,price_profile,
                  dispatch_limits,reserved_envelope,reservation_attempt_limit,
                  deadline_ms,created_ms)
             VALUES ('reservation:stale-version','target','issuer','epoch-1',
                     'client-key:stale-version','wave',1,'request:managed',?1,
                     'source:managed',?2,?3,?4,'project-policy','objective-policy',
                     'price-profile','v1:1024:10:1024:4096:1024',X'0e',1,999,100)",
            rusqlite::params![request_sha, source_sha, manifest_sha, trigger_sha],
        );
        assert!(
            stale_version_proof.is_err(),
            "proof creation must bind the expected current target version"
        );
        let stale_version_proof_count: i64 = migrated
            .read_connection()
            .query_row(
                "SELECT count(*) FROM local_resolution_reservation_proofs
                 WHERE reservation='reservation:stale-version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stale_version_proof_count, 0);
        migrated
            .connection()
            .execute(
                "INSERT INTO resolution_attempts
                     (decision_key,kind,members,paths,request_record_id,resolver,
                      charged_ms,attempt_number,request_sha256,input_manifest_sha256,
                      client_key,async_state)
                 VALUES ('decision:shared','failed_checks','lineage-a','',
                         'request:managed','local-managed-resolution',456,2,?1,?2,
                         'client-key','dispatching')",
                rusqlite::params![request_sha, manifest_sha],
            )
            .unwrap();
        let attempt_id = migrated.connection().last_insert_rowid();
        migrated
            .connection()
            .execute(
                "INSERT INTO local_resolution_reservation_proofs
                     (reservation,target,issuer,authority_epoch,client_key,wave,
                      expected_version,request_record_id,request_object_sha256,composition_source_record_id,
                      composition_source_object_sha256,input_manifest_object_sha256,
                      trigger_sha256,project_policy,objective_policy,price_profile,
                      dispatch_limits,reserved_envelope,reservation_attempt_limit,
                      deadline_ms,created_ms)
                 VALUES ('reservation','target','issuer','epoch-1','client-key','wave',0,
                         'request:managed',?1,'source:managed',?2,?3,?4,
                         'project-policy','objective-policy',
                         'price-profile','v1:1024:10:1024:4096:1024',X'04',1,999,100)",
                rusqlite::params![request_sha, source_sha, manifest_sha, trigger_sha],
            )
            .unwrap();
        migrated
            .connection()
            .execute(
                "INSERT INTO local_resolution_reservation_roots
                     (reservation,lineage_record_id) VALUES ('reservation','lineage-a')",
                [],
            )
            .unwrap();

        // Each invalid insert reaches the binding predicate for the intended
        // proof axis before the valid row is created below.
        assert_rejected_without_mutation(
            migrated.connection(),
            "reservation",
            "wrong shared comparable attempt number",
            |connection| {
                insert_binding(
                    connection,
                    "binding:wrong-shared-attempt",
                    "reservation",
                    attempt_id,
                    1,
                    1,
                )
            },
        );

        // Keep NEW.reservation_attempt_number valid and vary the persisted
        // reservation count, so the r.attempts equality guard is what rejects
        // the insert (rather than the binding column CHECK).
        migrated
            .connection()
            .execute(
                "UPDATE local_budget_reservations SET attempts=2
                 WHERE reservation='reservation'",
                [],
            )
            .unwrap();
        assert_rejected_without_mutation(
            migrated.connection(),
            "reservation",
            "wrong Local reservation attempt count",
            |connection| {
                insert_binding(
                    connection,
                    "binding:wrong-reservation-attempt",
                    "reservation",
                    attempt_id,
                    2,
                    1,
                )
            },
        );
        let attempts_after_rejection: i64 = migrated
            .connection()
            .query_row(
                "SELECT attempts FROM local_budget_reservations
                 WHERE reservation='reservation'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            attempts_after_rejection, 2,
            "the failed binding must not change the persisted Local attempt count"
        );
        migrated
            .connection()
            .execute(
                "UPDATE local_budget_reservations SET attempts=1
                 WHERE reservation='reservation'",
                [],
            )
            .unwrap();

        // The wrapper source pair is current-wave authority too. A matching
        // charged attempt cannot be bound to a different frozen source.
        migrated
            .connection()
            .execute(
                "INSERT INTO local_budget_reservations
                     (reservation,nonce,target,revision,wave,root,parent,body,
                      phase,spent,attempts,version)
                 VALUES ('reservation:wrong-source','nonce:wrong-source','target',
                         'revision','wave','reservation:wrong-source',NULL,X'09',
                         'completion_pending',X'00',1,1)",
                [],
            )
            .unwrap();
        migrated
            .connection()
            .execute(
                "INSERT INTO local_resolution_reservation_proofs
                     (reservation,target,issuer,authority_epoch,client_key,wave,
                      expected_version,request_record_id,request_object_sha256,composition_source_record_id,
                      composition_source_object_sha256,input_manifest_object_sha256,
                      trigger_sha256,project_policy,objective_policy,price_profile,
                      dispatch_limits,reserved_envelope,reservation_attempt_limit,
                      deadline_ms,created_ms)
                 VALUES ('reservation:wrong-source','target','issuer','epoch-1',
                         'client-key:wrong-source','wave',0,'request:managed',?1,
                         'source:wrong',?2,?3,?4,'project-policy','objective-policy',
                         'price-profile','v1:1024:10:1024:4096:1024',X'0a',1,999,100)",
                rusqlite::params![request_sha, &"0".repeat(64), manifest_sha, trigger_sha],
            )
            .unwrap();
        migrated
            .connection()
            .execute(
                "INSERT INTO local_resolution_reservation_roots
                     (reservation,lineage_record_id)
                 VALUES ('reservation:wrong-source','lineage-a')",
                [],
            )
            .unwrap();
        let wrong_source_attempt = insert_attempt(
            migrated.connection(),
            "request:managed",
            &request_sha,
            &manifest_sha,
            "client-key:wrong-source",
            3,
        )
        .unwrap();
        assert_rejected_without_mutation(
            migrated.connection(),
            "reservation:wrong-source",
            "wrong composition source wrapper identity",
            |connection| {
                insert_binding(
                    connection,
                    "binding:wrong-source",
                    "reservation:wrong-source",
                    wrong_source_attempt,
                    3,
                    1,
                )
            },
        );

        migrated
            .connection()
            .execute(
                "INSERT INTO local_budget_reservations
                     (reservation,nonce,target,revision,wave,root,parent,body,
                      phase,spent,attempts,version)
                 VALUES ('reservation:wrong-source-object','nonce:wrong-source-object',
                         'target','revision','wave','reservation:wrong-source-object',
                         NULL,X'0b','completion_pending',X'00',1,1)",
                [],
            )
            .unwrap();
        migrated
            .connection()
            .execute(
                "INSERT INTO local_resolution_reservation_proofs
                     (reservation,target,issuer,authority_epoch,client_key,wave,
                      expected_version,request_record_id,request_object_sha256,composition_source_record_id,
                      composition_source_object_sha256,input_manifest_object_sha256,
                      trigger_sha256,project_policy,objective_policy,price_profile,
                      dispatch_limits,reserved_envelope,reservation_attempt_limit,
                      deadline_ms,created_ms)
                 VALUES ('reservation:wrong-source-object','target','issuer','epoch-1',
                         'client-key:wrong-source-object','wave',0,'request:managed',?1,
                         'source:managed',?2,?3,?4,'project-policy','objective-policy',
                         'price-profile','v1:1024:10:1024:4096:1024',X'0c',1,999,100)",
                rusqlite::params![request_sha, &"0".repeat(64), manifest_sha, trigger_sha],
            )
            .unwrap();
        migrated
            .connection()
            .execute(
                "INSERT INTO local_resolution_reservation_roots
                     (reservation,lineage_record_id)
                 VALUES ('reservation:wrong-source-object','lineage-a')",
                [],
            )
            .unwrap();
        let wrong_source_object_attempt = insert_attempt(
            migrated.connection(),
            "request:managed",
            &request_sha,
            &manifest_sha,
            "client-key:wrong-source-object",
            3,
        )
        .unwrap();
        assert_rejected_without_mutation(
            migrated.connection(),
            "reservation:wrong-source-object",
            "wrong composition source object digest",
            |connection| {
                insert_binding(
                    connection,
                    "binding:wrong-source-object",
                    "reservation:wrong-source-object",
                    wrong_source_object_attempt,
                    3,
                    1,
                )
            },
        );

        let wrong_request_record_attempt = insert_attempt(
            migrated.connection(),
            "request:other",
            &request_sha,
            &manifest_sha,
            "client-key",
            2,
        )
        .unwrap();
        assert_rejected_without_mutation(
            migrated.connection(),
            "reservation",
            "wrong request record ID",
            |connection| {
                insert_binding(
                    connection,
                    "binding:wrong-request-record",
                    "reservation",
                    wrong_request_record_attempt,
                    2,
                    1,
                )
            },
        );

        let wrong_request_object_attempt = insert_attempt(
            migrated.connection(),
            "request:managed",
            &"d".repeat(64),
            &manifest_sha,
            "client-key",
            2,
        )
        .unwrap();
        assert_rejected_without_mutation(
            migrated.connection(),
            "reservation",
            "wrong request object digest",
            |connection| {
                insert_binding(
                    connection,
                    "binding:wrong-request-object",
                    "reservation",
                    wrong_request_object_attempt,
                    2,
                    1,
                )
            },
        );

        let wrong_manifest_object_attempt = insert_attempt(
            migrated.connection(),
            "request:managed",
            &request_sha,
            &"e".repeat(64),
            "client-key",
            2,
        )
        .unwrap();
        assert_rejected_without_mutation(
            migrated.connection(),
            "reservation",
            "wrong manifest object digest",
            |connection| {
                insert_binding(
                    connection,
                    "binding:wrong-manifest-object",
                    "reservation",
                    wrong_manifest_object_attempt,
                    2,
                    1,
                )
            },
        );

        let manual_attempt = insert_attempt_with_route(
            migrated.connection(),
            "request:managed",
            &request_sha,
            &manifest_sha,
            "client-key",
            "manual",
            None,
            2,
        )
        .unwrap();
        assert_rejected_without_mutation(
            migrated.connection(),
            "reservation",
            "manual attempt cannot receive a managed binding",
            |connection| {
                insert_binding(
                    connection,
                    "binding:manual-attempt",
                    "reservation",
                    manual_attempt,
                    2,
                    1,
                )
            },
        );

        let labeled_managed_attempt = insert_attempt_with_route(
            migrated.connection(),
            "request:managed",
            &request_sha,
            &manifest_sha,
            "client-key",
            "local-managed-resolution",
            Some("manual-session"),
            2,
        )
        .unwrap();
        assert_rejected_without_mutation(
            migrated.connection(),
            "reservation",
            "session-labeled attempt cannot receive a managed binding",
            |connection| {
                insert_binding(
                    connection,
                    "binding:labeled-attempt",
                    "reservation",
                    labeled_managed_attempt,
                    2,
                    1,
                )
            },
        );

        migrated
            .connection()
            .execute(
                "INSERT INTO local_budget_reservations
                     (reservation,nonce,target,revision,wave,root,parent,body,
                      phase,spent,attempts,version)
                 VALUES ('reservation:no-roots','nonce:no-roots','target','revision',
                         'wave','reservation:no-roots',NULL,X'07',
                         'completion_pending',X'00',1,1)",
                [],
            )
            .unwrap();
        migrated
            .connection()
            .execute(
                "INSERT INTO local_resolution_reservation_proofs
                     (reservation,target,issuer,authority_epoch,client_key,wave,
                      expected_version,request_record_id,request_object_sha256,composition_source_record_id,
                      composition_source_object_sha256,input_manifest_object_sha256,
                      trigger_sha256,project_policy,objective_policy,price_profile,
                      dispatch_limits,reserved_envelope,reservation_attempt_limit,
                      deadline_ms,created_ms)
                 VALUES ('reservation:no-roots','target','issuer','epoch-1',
                         'client-key:no-roots','wave',0,'request:managed',?1,
                         'source:managed',?2,?3,?4,
                         'project-policy','objective-policy','price-profile',
                         'v1:1024:10:1024:4096:1024',X'08',1,999,100)",
                rusqlite::params![request_sha, source_sha, manifest_sha, trigger_sha],
            )
            .unwrap();
        let no_roots_attempt = insert_attempt(
            migrated.connection(),
            "request:managed",
            &request_sha,
            &manifest_sha,
            "client-key:no-roots",
            2,
        )
        .unwrap();
        assert_rejected_without_mutation(
            migrated.connection(),
            "reservation:no-roots",
            "binding without selected lineage roots",
            |connection| {
                insert_binding(
                    connection,
                    "binding:no-roots",
                    "reservation:no-roots",
                    no_roots_attempt,
                    2,
                    1,
                )
            },
        );

        // Recheck reservation and wave identity at binding time too. The
        // proof was valid when inserted; these direct fixture mutations model
        // stale or corrupted state reaching the durable trigger boundary.
        migrated
            .connection()
            .execute_batch(
                "UPDATE local_targets SET active_revision='revision:other'
                 WHERE target='target';
                 UPDATE local_budget_reservations SET revision='revision:other'
                 WHERE reservation='reservation';",
            )
            .unwrap();
        assert_rejected_without_mutation(
            migrated.connection(),
            "reservation",
            "wave revision must still match the reservation revision",
            |connection| {
                insert_binding(
                    connection,
                    "binding:stale-reservation-revision",
                    "reservation",
                    attempt_id,
                    2,
                    1,
                )
            },
        );
        migrated
            .connection()
            .execute_batch(
                "UPDATE local_targets SET active_revision='revision'
                 WHERE target='target';
                 UPDATE local_budget_reservations SET revision='revision'
                 WHERE reservation='reservation';
                 UPDATE local_waves SET target='target:other' WHERE wave='wave';",
            )
            .unwrap();
        assert_rejected_without_mutation(
            migrated.connection(),
            "reservation",
            "wave target must still match the proof target",
            |connection| {
                insert_binding(
                    connection,
                    "binding:stale-wave-target",
                    "reservation",
                    attempt_id,
                    2,
                    1,
                )
            },
        );
        migrated
            .connection()
            .execute(
                "UPDATE local_waves SET target='target' WHERE wave='wave'",
                [],
            )
            .unwrap();

        insert_binding(
            migrated.connection(),
            "binding",
            "reservation",
            attempt_id,
            2,
            1,
        )
        .unwrap();
        let counters: (i64, i64) = migrated
            .read_connection()
            .query_row(
                "SELECT shared_attempt_number,reservation_attempt_number
                 FROM local_managed_resolution_attempt_bindings WHERE binding_id='binding'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(counters, (2, 1));

        let duplicate_binding = migrated.connection().execute(
            "INSERT INTO local_managed_resolution_attempt_bindings
                 (binding_id,reservation,attempt_id,authority_epoch,
                  reservation_version,shared_attempt_number,
                  reservation_attempt_number,route,created_ms)
             VALUES ('binding:duplicate','reservation',?1,'epoch-1',1,2,1,
                     'local_managed_resolution_v1',102)",
            [attempt_id],
        );
        assert!(
            duplicate_binding.is_err(),
            "a managed reservation binds once"
        );
        let changed_proof = migrated.connection().execute(
            "UPDATE local_resolution_reservation_proofs
             SET composition_source_record_id='source:changed'
             WHERE reservation='reservation'",
            [],
        );
        assert!(
            changed_proof.is_err(),
            "the exact request/source proof is immutable"
        );
        let changed_request_proof = migrated.connection().execute(
            "UPDATE local_resolution_reservation_proofs
             SET request_record_id='request:changed' WHERE reservation='reservation'",
            [],
        );
        assert!(
            changed_request_proof.is_err(),
            "the exact request proof is immutable"
        );
        let appended_root = migrated.connection().execute(
            "INSERT INTO local_resolution_reservation_roots
                 (reservation,lineage_record_id) VALUES ('reservation','lineage-b')",
            [],
        );
        assert!(
            appended_root.is_err(),
            "the selected root set closes at binding"
        );
        migrated
            .connection()
            .execute(
                "INSERT INTO local_budget_reservations
                     (reservation,nonce,target,revision,wave,root,parent,body,
                      phase,spent,attempts,version)
                 VALUES ('reservation:retry','nonce:retry','target','revision','wave',
                         'reservation:retry',NULL,X'05','reserved',X'00',0,0)",
                [],
            )
            .unwrap();
        let cross_epoch_key = migrated.connection().execute(
            "INSERT INTO local_resolution_reservation_proofs
                 (reservation,target,issuer,authority_epoch,client_key,wave,
                  expected_version,request_record_id,request_object_sha256,composition_source_record_id,
                  composition_source_object_sha256,input_manifest_object_sha256,
                  trigger_sha256,project_policy,objective_policy,price_profile,
                  dispatch_limits,reserved_envelope,reservation_attempt_limit,
                  deadline_ms,created_ms)
             VALUES ('reservation:retry','target','issuer','epoch-2','client-key','wave',0,
                     'request:other',?1,'source:other',?2,?3,?4,
                     'project-policy','objective-policy',
                     'price-profile','v1:1024:10:1024:4096:1024',X'06',1,999,102)",
            rusqlite::params![request_sha, source_sha, manifest_sha, trigger_sha],
        );
        assert!(
            cross_epoch_key.is_err(),
            "idempotency scope remains stable across authority epochs"
        );
    }

    #[test]
    fn schema_28_migrates_existing_waves_with_null_result_references() {
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path(), &[]).unwrap();
        store
            .connection()
            .execute_batch(
                "INSERT INTO local_targets (target, issuer) VALUES ('target', 'issuer');
                 INSERT INTO local_target_revisions (revision, target, body)
                     VALUES ('revision', 'target', X'01');
                 INSERT INTO local_waves
                     (wave, target, revision, sequence, body, phase,
                      resolution_request_record_id, resolution_request_object)
                     VALUES ('wave', 'target', 'revision', 1, X'02', 'verifying',
                             'request:legacy', '1111111111111111111111111111111111111111111111111111111111111111');",
            )
            .unwrap();
        drop_resolution_schema_28_for_legacy_fixture(store.connection()).unwrap();
        store
            .connection()
            .execute_batch(
                "UPDATE meta SET value = '27'
                 WHERE key IN ('schema_version', 'min_compatible_schema');",
            )
            .unwrap();
        drop(store);

        let migrated = open(host.path(), &[]).unwrap();
        let wave: (
            String,
            String,
            String,
            String,
            Vec<u8>,
            Option<String>,
            Option<String>,
        ) = migrated
            .read_connection()
            .query_row(
                "SELECT wave,target,revision,phase,body,
                            resolution_result_record_id,resolution_result_object
                     FROM local_waves WHERE wave='wave'",
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
            wave,
            (
                "wave".into(),
                "target".into(),
                "revision".into(),
                "verifying".into(),
                vec![2],
                None,
                None,
            )
        );
        assert_eq!(
            migrated.schema_version(),
            COLLABORATION_STATE_SCHEMA_VERSION
        );
    }

    #[test]
    fn schema_28_rebuild_preserves_native_gate_jobs_roots_and_foreign_keys() {
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path(), &[]).unwrap();
        store
            .connection()
            .execute_batch(
                "INSERT INTO local_targets (target, issuer) VALUES ('target', 'issuer');
                 INSERT INTO local_targets (target, issuer) VALUES ('target:unknown', 'issuer');
                 INSERT INTO local_target_revisions (revision, target, body)
                     VALUES ('revision', 'target', X'01');
                 INSERT INTO local_target_revisions (revision, target, body)
                     VALUES ('revision:unknown', 'target:unknown', X'07');
                 INSERT INTO local_waves
                     (wave, target, revision, sequence, body, phase)
                     VALUES ('wave', 'target', 'revision', 1, X'02', 'verifying');
                 INSERT INTO local_waves
                     (wave, target, revision, sequence, body, phase)
                     VALUES ('wave:unknown', 'target:unknown', 'revision:unknown', 1, X'08', 'verifying');

                 DROP TRIGGER local_native_gate_no_dispatch_after_root_release;
                 DROP INDEX local_native_gate_jobs_by_wave;
                 DROP TABLE local_native_gate_roots;
                 DROP TABLE local_native_gate_jobs;
                 CREATE TABLE local_native_gate_jobs (
                     id TEXT PRIMARY KEY NOT NULL,
                     wave TEXT NOT NULL UNIQUE REFERENCES local_waves(wave),
                     target TEXT NOT NULL, revision TEXT NOT NULL, epoch TEXT NOT NULL,
                     body BLOB NOT NULL,
                     state TEXT NOT NULL CHECK(state IN ('prepared','dispatching','completed','unknown')),
                     observation BLOB,
                     roots_released_ms INTEGER
                 ) STRICT;
                 CREATE TABLE local_native_gate_roots (
                     job TEXT NOT NULL REFERENCES local_native_gate_jobs(id),
                     kind TEXT NOT NULL, reference TEXT NOT NULL,
                     PRIMARY KEY(job,kind,reference)
                 ) STRICT;
                 CREATE TRIGGER local_native_gate_no_dispatch_after_root_release
                 BEFORE UPDATE OF state ON local_native_gate_jobs
                 WHEN OLD.roots_released_ms IS NOT NULL AND NEW.state<>OLD.state
                 BEGIN SELECT RAISE(ABORT, 'native gate roots were released'); END;
                 INSERT INTO local_native_gate_jobs
                     (id,wave,target,revision,epoch,body,state,observation,roots_released_ms)
                     VALUES ('job:legacy','wave','target','revision','epoch',X'0304',
                             'completed',X'0506',1234);
                 INSERT INTO local_native_gate_roots VALUES
                     ('job:legacy','snapshot','sha256:subject'),
                     ('job:legacy','object','0123456789abcdef');
                 INSERT INTO local_native_gate_jobs
                     (id,wave,target,revision,epoch,body,state,observation,roots_released_ms)
                     VALUES ('job:unknown','wave:unknown','target:unknown','revision:unknown','epoch',X'090a',
                             'unknown',X'0b0c',NULL);
                 INSERT INTO local_native_gate_roots VALUES
                     ('job:unknown','snapshot','sha256:unknown');",
            )
            .unwrap();
        drop_resolution_schema_28_for_legacy_fixture(store.connection()).unwrap();
        store
            .connection()
            .execute_batch(
                "UPDATE meta SET value = '27'
                 WHERE key IN ('schema_version', 'min_compatible_schema');",
            )
            .unwrap();
        drop(store);

        let mut migrated = open(host.path(), &[]).unwrap();
        #[derive(Debug, PartialEq, Eq)]
        struct LegacyGateJob {
            id: String,
            wave: String,
            target: String,
            revision: String,
            epoch: String,
            body: Vec<u8>,
            state: String,
            observation: Option<Vec<u8>>,
            roots_released_ms: Option<i64>,
        }
        let job = migrated
            .read_connection()
            .query_row(
                "SELECT id,wave,target,revision,epoch,body,state,observation,roots_released_ms
                 FROM local_native_gate_jobs WHERE id='job:legacy'",
                [],
                |row| {
                    Ok(LegacyGateJob {
                        id: row.get(0)?,
                        wave: row.get(1)?,
                        target: row.get(2)?,
                        revision: row.get(3)?,
                        epoch: row.get(4)?,
                        body: row.get(5)?,
                        state: row.get(6)?,
                        observation: row.get(7)?,
                        roots_released_ms: row.get(8)?,
                    })
                },
            )
            .unwrap();
        assert_eq!(
            job,
            LegacyGateJob {
                id: "job:legacy".into(),
                wave: "wave".into(),
                target: "target".into(),
                revision: "revision".into(),
                epoch: "epoch".into(),
                body: vec![3, 4],
                state: "completed".into(),
                observation: Some(vec![5, 6]),
                roots_released_ms: Some(1234),
            }
        );
        let roots: Vec<(String, String, String)> = migrated
            .read_connection()
            .prepare(
                "SELECT job,kind,reference FROM local_native_gate_roots ORDER BY kind,reference",
            )
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            roots,
            vec![
                (
                    "job:legacy".into(),
                    "object".into(),
                    "0123456789abcdef".into()
                ),
                (
                    "job:legacy".into(),
                    "snapshot".into(),
                    "sha256:subject".into()
                ),
                (
                    "job:unknown".into(),
                    "snapshot".into(),
                    "sha256:unknown".into()
                ),
            ]
        );
        let unknown_job: (String, Option<Vec<u8>>, Option<i64>) = migrated
            .read_connection()
            .query_row(
                "SELECT state,observation,roots_released_ms FROM local_native_gate_jobs
                 WHERE id='job:unknown'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(unknown_job, ("unknown".into(), Some(vec![11, 12]), None));
        assert!(
            migrated
                .connection()
                .execute(
                    "UPDATE local_native_gate_jobs SET state='unknown' WHERE id='job:legacy'",
                    [],
                )
                .is_err()
        );
        let root_fk_target: String = migrated
            .read_connection()
            .query_row(
                "SELECT \"table\" FROM pragma_foreign_key_list('local_native_gate_roots')
                 WHERE \"from\"='job'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(root_fk_target, "local_native_gate_jobs");
        let wave_index_unique: i64 = migrated
            .read_connection()
            .query_row(
                "SELECT \"unique\" FROM pragma_index_list('local_native_gate_jobs')
                 WHERE name='local_native_gate_jobs_by_wave'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(wave_index_unique, 0);
        let violations: i64 = migrated
            .read_connection()
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
        migrated
            .connection()
            .execute(
                "INSERT INTO local_native_gate_jobs
                     (id,wave,target,revision,epoch,body,state)
                 VALUES ('job:new-spec','wave','target','revision','epoch',X'07','prepared')",
                [],
            )
            .unwrap();
        drop(migrated);

        let mut reopened = open(host.path(), &[]).unwrap();
        let jobs: i64 = reopened
            .read_connection()
            .query_row(
                "SELECT count(*) FROM local_native_gate_jobs WHERE wave='wave'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(jobs, 2);
        let unknown_job: (String, Option<Vec<u8>>, Option<i64>) = reopened
            .read_connection()
            .query_row(
                "SELECT state,observation,roots_released_ms FROM local_native_gate_jobs
                 WHERE id='job:unknown'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(unknown_job, ("unknown".into(), Some(vec![11, 12]), None));
        assert!(
            reopened
                .connection()
                .execute(
                    "UPDATE local_native_gate_jobs SET state='unknown' WHERE id='job:legacy'",
                    [],
                )
                .is_err()
        );
        let violations: i64 = reopened
            .read_connection()
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
    }

    #[test]
    fn schema_28_result_references_are_paired_immutable_and_fixture_downgrade_refuses_data() {
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path(), &[]).unwrap();
        store
            .connection()
            .execute_batch(
                "INSERT INTO local_targets (target, issuer) VALUES ('target', 'issuer');
                 INSERT INTO local_target_revisions (revision, target, body)
                     VALUES ('revision', 'target', X'01');
                 INSERT INTO local_waves
                     (wave, target, revision, sequence, body, phase,
                      candidate_snapshot, candidate_commit,
                      resolution_request_record_id, resolution_request_object)
                     VALUES ('wave', 'target', 'revision', 1, X'02', 'verifying',
                             'snapshot:1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                             'request:1', '1111111111111111111111111111111111111111111111111111111111111111');",
            )
            .unwrap();
        assert!(
            store
                .connection()
                .execute(
                    "UPDATE local_waves SET resolution_result_record_id='result' WHERE wave='wave'",
                    [],
                )
                .is_err()
        );
        assert!(store
            .connection()
            .execute(
                "UPDATE local_waves
                 SET resolution_request_record_id=NULL,
                     resolution_request_object=NULL,
                     resolution_result_record_id='result',
                     resolution_result_object='2222222222222222222222222222222222222222222222222222222222222222'
                 WHERE wave='wave'",
                [],
            )
            .is_err());
        store
            .connection()
            .execute(
                "UPDATE local_waves
                 SET resolution_result_record_id='result:1',
                     resolution_result_object='2222222222222222222222222222222222222222222222222222222222222222'
                 WHERE wave='wave'",
                [],
            )
            .unwrap();
        for statement in [
            "UPDATE local_waves SET resolution_result_record_id=NULL, resolution_result_object=NULL WHERE wave='wave'",
            "UPDATE local_waves SET resolution_result_record_id='result:2' WHERE wave='wave'",
            "UPDATE local_waves SET resolution_request_record_id='request:2' WHERE wave='wave'",
            "UPDATE local_waves SET candidate_snapshot='snapshot:2' WHERE wave='wave'",
        ] {
            assert!(
                store.connection().execute(statement, []).is_err(),
                "attached result and its request binding are immutable"
            );
        }

        let downgrade = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop_resolution_schema_28_for_legacy_fixture(store.connection())
        }));
        assert!(
            downgrade.is_err(),
            "fixture downgrade must refuse linked results"
        );
        let preserved: (Option<String>, Option<String>) = store
            .read_connection()
            .query_row(
                "SELECT resolution_result_record_id,resolution_result_object
                 FROM local_waves WHERE wave='wave'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            preserved,
            (
                Some("result:1".into()),
                Some("2222222222222222222222222222222222222222222222222222222222222222".into()),
            )
        );
        assert!(
            fixture_column_exists(
                store.connection(),
                "local_waves",
                "resolution_result_record_id"
            )
            .unwrap()
        );
        let result_trigger_count: i64 = store
            .read_connection()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='trigger' AND name='local_waves_resolution_result_pair_update'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(result_trigger_count, 1);
    }

    #[test]
    fn schema_27_preserves_unsettled_legacy_attempts_as_unknown() {
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path(), &[]).unwrap();
        drop_resolution_schema_27_for_legacy_fixture(store.connection()).unwrap();
        store
            .connection()
            .execute(
                "UPDATE meta SET value='26'
                 WHERE key IN ('schema_version','min_compatible_schema')",
                [],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO resolution_attempts
                 (decision_key,kind,members,paths,request_record_id,resolver,outcome,charged_ms)
                 VALUES('legacy-decision','conflict','member','path','req:legacy',
                        'legacy-resolver',NULL,1)",
                [],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO resolution_attempts
                 (decision_key,kind,members,paths,request_record_id,resolver,outcome,charged_ms)
                 VALUES('legacy-terminal','conflict','member','path','req:terminal',
                        'legacy-resolver','resolved',2)",
                [],
            )
            .unwrap();
        let stale_writer_path = store.project_dir().join("state.db");
        drop(store);
        let stale_writer = rusqlite::Connection::open(stale_writer_path).unwrap();
        let stale_insert = "INSERT INTO resolution_attempts
             (decision_key,kind,members,paths,request_record_id,resolver,charged_ms)
             VALUES(?1,?2,?3,?4,?5,?6,?7)";
        drop(stale_writer.prepare_cached(stale_insert).unwrap());

        let mut migrated = open(host.path(), &[]).unwrap();
        let (state, outcome): (String, Option<String>) = migrated
            .read_connection()
            .query_row(
                "SELECT async_state,outcome FROM resolution_attempts
                 WHERE decision_key='legacy-decision'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, "outcome_unknown");
        assert_eq!(outcome, None);
        let (terminal_state, terminal_outcome): (String, Option<String>) = migrated
            .read_connection()
            .query_row(
                "SELECT async_state,outcome FROM resolution_attempts
                 WHERE decision_key='legacy-terminal'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(terminal_state, "settled");
        assert_eq!(terminal_outcome.as_deref(), Some("resolved"));
        assert_eq!(
            migrated.schema_version(),
            COLLABORATION_STATE_SCHEMA_VERSION
        );

        let inserted_v27 = migrated
            .connection()
            .execute(
                "INSERT INTO resolution_attempts
                 (decision_key,kind,members,paths,request_record_id,resolver,charged_ms,
                  async_token,client_key,session_label,attempt_number,request_sha256,input_sha256,
                  input_manifest_sha256,dispatch_limits,async_state)
                 VALUES('new-async','conflict','member','path','req:new','external-session-label',
                        3,'attempt:new','client:new','session:new',1,?1,?1,?1,
                        'v1:1024:8:1024:4096:1024','exported')",
                rusqlite::params!["0".repeat(64)],
            )
            .unwrap();
        assert_eq!(inserted_v27, 1);

        let first_response = "2".repeat(64);
        let substituted_response = "3".repeat(64);
        migrated
            .connection()
            .execute(
                "UPDATE resolution_attempts SET async_state='dispatching'
                 WHERE async_token='attempt:new'",
                [],
            )
            .unwrap();
        migrated
            .connection()
            .execute(
                "UPDATE resolution_attempts
                 SET async_state='importing',response_sha256=?1
                 WHERE async_token='attempt:new'",
                [&first_response],
            )
            .unwrap();
        assert!(
            migrated
                .connection()
                .execute(
                    "UPDATE resolution_attempts
                     SET async_state='settled',outcome='resolved',response_sha256=?1
                     WHERE async_token='attempt:new'",
                    [&substituted_response],
                )
                .is_err(),
            "settlement cannot replace the response digest fixed at import"
        );
        let bound_response: (String, String, Option<String>) = migrated
            .read_connection()
            .query_row(
                "SELECT async_state,response_sha256,outcome FROM resolution_attempts
                 WHERE async_token='attempt:new'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            bound_response,
            ("importing".into(), first_response, None),
            "rejected digest substitution leaves the bound import untouched"
        );
        assert!(
            migrated
                .connection()
                .execute(
                    "INSERT INTO resolution_attempts
                     (decision_key,kind,members,paths,request_record_id,resolver,charged_ms,
                      async_token,client_key,session_label,attempt_number,request_sha256,input_sha256,
                      input_manifest_sha256,dispatch_limits,async_state)
                     VALUES('direct-dispatch','conflict','member','path','req:new',
                            'external-session-label',4,'attempt:direct','client:direct','session:new',
                            2,?1,?1,?1,'v1:1024:8:1024:4096:1024','dispatching')",
                    rusqlite::params!["1".repeat(64)],
                )
                .is_err()
        );

        assert!(
            stale_writer
                .prepare_cached(stale_insert)
                .unwrap()
                .execute(rusqlite::params![
                    "stale-writer",
                    "conflict",
                    "member",
                    "path",
                    "req:stale",
                    "old",
                    2
                ])
                .is_err()
        );
        assert!(
            stale_writer
                .execute(
                    "UPDATE resolution_attempts SET outcome='resolved'
                 WHERE decision_key='legacy-decision'",
                    [],
                )
                .is_err()
        );
        assert!(
            stale_writer
                .execute(
                    "DELETE FROM resolution_attempts WHERE decision_key='legacy-decision'",
                    [],
                )
                .is_err()
        );
        assert!(
            stale_writer
                .execute(
                    "UPDATE resolution_attempts SET outcome='unresolved'
                     WHERE decision_key='legacy-terminal'",
                    [],
                )
                .is_err()
        );
        assert!(
            stale_writer
                .execute(
                    "UPDATE resolution_attempts SET outcome='resolved'
                     WHERE decision_key='new-async'",
                    [],
                )
                .is_err()
        );
        let final_legacy: (String, Option<String>) = migrated
            .read_connection()
            .query_row(
                "SELECT async_state,outcome FROM resolution_attempts
                 WHERE decision_key='legacy-decision'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(final_legacy, ("outcome_unknown".into(), None));
    }

    /// Inspection opens read-only: it reports a store that needs migration
    /// without migrating it, refuses a newer floor and a foreign project, and
    /// leaves every stored byte as it was.
    #[test]
    fn inspection_reads_without_migrating_or_writing() {
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path(), &[]).unwrap();
        drop_resolution_schema_27_for_legacy_fixture(store.connection()).unwrap();
        crate::collaboration_sensitive::drop_sensitive_response_tables_for_legacy_fixture(
            store.connection(),
        )
        .unwrap();
        store
            .connection()
            .execute_batch(
                "DROP TABLE IF EXISTS reclaimed_contribution_revisions; DROP TABLE IF EXISTS contribution_revision_identity_bases; DROP TABLE IF EXISTS contribution_revision_heads; DROP TABLE IF EXISTS contribution_revisions; DROP TABLE IF EXISTS local_logical_contributions; DROP TABLE IF EXISTS local_project_file_ids; DROP TABLE IF EXISTS local_project_bindings; DROP INDEX IF EXISTS capture_receipts_operation_lineage; DROP TABLE IF EXISTS contribution_briefs; DROP TABLE IF EXISTS local_capture_notice_outcomes; DROP TABLE IF EXISTS local_capture_notices; DROP TABLE local_native_gate_roots; DROP TABLE local_native_gate_jobs; DROP TABLE local_import_rehearsals; DROP TABLE local_imported_records; DROP TABLE local_imported_sources; DROP TABLE local_imports; DROP TABLE local_adoption_receipts; DROP TABLE local_agent_dispatches; DROP TABLE local_agent_reconciliations; DROP TABLE local_handoffs; DROP TABLE local_agent_sessions; DROP TABLE local_exchange_messages; DROP TABLE local_exchange_deliveries; DROP TABLE local_exchange_streams; DROP TABLE local_exchange_events; DROP TABLE local_exchange_projection; DROP TABLE local_exchange_observations; DROP TABLE local_exchange_bindings; DROP TRIGGER outbox_capture_insert; DROP TRIGGER outbox_capture_update; DROP TRIGGER outbox_capture_delete; DROP TABLE local_generation_roots; DROP TABLE local_generation_receipts; DROP TABLE local_generations; DROP TABLE local_acceptance_qualifications; DROP TABLE local_acceptance_receipts; DROP TABLE local_acceptance_bindings; DROP TABLE local_source_transport_receipts; DROP TABLE local_source_transports; DROP TABLE local_verification_evidence; DROP TABLE local_verifier_producers; DROP TABLE local_authority_state; DROP TABLE local_wave_arrivals;
                 DROP TABLE local_waves;
                 UPDATE meta SET value = '2' WHERE key = 'schema_version';",
            )
            .unwrap();
        let project_dir = store.project_dir().to_path_buf();
        drop(store);
        let database = project_dir.join("state.db");
        let before = std::fs::read(&database).unwrap();

        let snapshot = inspect_at(project_dir.clone(), &key("proj-a")).unwrap();
        assert_eq!(snapshot.schema_version, 2);
        assert!(snapshot.needs_migration);
        assert!(!snapshot.has_table("no_such_table"));
        assert!(snapshot.has_table("meta"));
        drop(snapshot);
        assert_eq!(
            std::fs::read(&database).unwrap(),
            before,
            "inspection wrote"
        );

        let error = inspect_at(project_dir.clone(), &key("proj-b")).unwrap_err();
        assert_eq!(error.code(), "project_mismatch", "{error}");

        let mut store = open(host.path(), &[]).unwrap();
        store
            .connection()
            .execute(
                "UPDATE meta SET value = '999' WHERE key = 'min_compatible_schema'",
                [],
            )
            .unwrap();
        drop(store);
        let error = inspect_at(project_dir, &key("proj-a")).unwrap_err();
        assert_eq!(error.code(), "schema_too_new", "{error}");
    }

    const CRASH_CHILD: &str = "AETHYME_COLLABORATION_CRASH_CHILD";

    /// Run only as the child of the crash test: commit numbered rows, one per
    /// transaction, and report each after its commit returned.
    #[test]
    #[ignore = "child process of a_killed_writer_loses_no_acknowledged_commit"]
    fn crash_child_commits_until_killed() {
        let Some(host) = std::env::var_os(CRASH_CHILD) else {
            return;
        };
        let mut store = open(Path::new(&host), &[]).unwrap();
        let connection = store.connection();
        connection
            .execute_batch("CREATE TABLE probe (n INTEGER PRIMARY KEY, pad BLOB NOT NULL) STRICT")
            .unwrap();
        let stdout = std::io::stdout();
        for n in 1_i64.. {
            connection
                .execute(
                    "INSERT INTO probe (n, pad) VALUES (?1, zeroblob(4096))",
                    [n],
                )
                .unwrap();
            let mut out = stdout.lock();
            std::io::Write::write_all(&mut out, format!("committed {n}\n").as_bytes()).unwrap();
            std::io::Write::flush(&mut out).unwrap();
        }
    }

    /// T08 at the state layer: a writer killed between and during commits
    /// leaves an intact database holding every commit it acknowledged, and
    /// nothing out of order. (Power loss is a documented assumption, not
    /// something a test can produce.)
    #[test]
    fn a_killed_writer_loses_no_acknowledged_commit() {
        let host = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "collaboration_state::tests::crash_child_commits_until_killed",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CRASH_CHILD, host.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut acknowledged = 0_i64;
        for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines() {
            if let Some(n) = line.unwrap().strip_prefix("committed ") {
                acknowledged = n.parse().unwrap();
                if acknowledged >= 300 {
                    break;
                }
            }
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            acknowledged >= 300,
            "the child stopped early at {acknowledged}"
        );

        let store = open(host.path(), &[]).unwrap();
        let integrity: String = store
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
        let (count, max): (i64, i64) = store
            .connection
            .query_row("SELECT count(*), max(n) FROM probe", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert!(
            max >= acknowledged,
            "lost acknowledged commits: {max} < {acknowledged}"
        );
        assert_eq!(count, max, "commits are contiguous");
    }
}
