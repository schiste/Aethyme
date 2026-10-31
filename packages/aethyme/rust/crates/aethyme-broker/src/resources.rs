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
pub(crate) use managed_storage_inventory::{ManagedStorageOwner, ManagedStorageRootSetBinding};

pub const HOST_RESOURCE_SCHEMA_VERSION: u32 = 1;
/// Additive ledger version, independent of the host-resource store version.
pub(crate) const MANAGED_STORAGE_SCHEMA_VERSION: u32 = 4;
pub const HOST_RESOURCE_REQUEST_SCHEMA_VERSION: u32 = 1;
const HOST_RESOURCE_STORE_SCHEMA_VERSION: u32 = 2;
const MIN_TTL_SECONDS: u64 = 15;
const MAX_TTL_SECONDS: u64 = 86_400;
/// Bound each repository's durable managed-storage ledger, including policy
/// rows and released claim tombstones retained for idempotency. This is a
/// per-repository bound; it does not constrain unrelated repositories sharing
/// the host resource database.
const MAX_MANAGED_STORAGE_ROWS_PER_REPOSITORY: i64 = 16_384;
const ORPHANED_HOLDER_CONFLICT_CODE: &str = "orphaned_holder";

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

/// Managed-storage migrations are additive. They never rewrite or drop legacy
/// claims, lineage, or policy rows; admission and producer state live in
/// separate tables.
const MANAGED_STORAGE_GROUP_SCHEMA: &str = r#"
CREATE TABLE managed_storage_claim_groups (
 repository TEXT NOT NULL,
 group_id TEXT NOT NULL,
 request_id TEXT NOT NULL,
 lease_id TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK (generation > 0),
 materialization TEXT NOT NULL,
 root_set_digest TEXT NOT NULL,
 pre_inventory_digest TEXT NOT NULL,
 member_manifest_digest TEXT NOT NULL,
 manifest_digest TEXT NOT NULL,
 member_count INTEGER NOT NULL CHECK (member_count BETWEEN 1 AND 32),
 state TEXT NOT NULL CHECK (state IN ('reserved','uncertain','settled')),
 receipt_digest TEXT,
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,
 PRIMARY KEY (repository, group_id),
 UNIQUE (repository, request_id),
 UNIQUE (repository, member_manifest_digest)
);
CREATE INDEX managed_storage_groups_by_request
 ON managed_storage_claim_groups(repository, request_id);
CREATE TABLE managed_storage_claim_group_members (
 repository TEXT NOT NULL,
 group_id TEXT NOT NULL,
 ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 0 AND 31),
 filesystem_id TEXT NOT NULL,
 claim_id TEXT NOT NULL,
 root_identity TEXT NOT NULL,
 claim_request_digest TEXT NOT NULL,
 requested_bytes INTEGER NOT NULL CHECK (requested_bytes > 0),
 reserved_bytes INTEGER NOT NULL CHECK (reserved_bytes > 0),
 predecessor_claim_id TEXT,
 baseline_bytes INTEGER NOT NULL CHECK (baseline_bytes >= 0),
 root_instance_id TEXT,
 PRIMARY KEY (repository, group_id, ordinal),
 UNIQUE (repository, filesystem_id, claim_id),
 UNIQUE (repository, group_id, filesystem_id, root_identity),
 FOREIGN KEY (repository, group_id)
   REFERENCES managed_storage_claim_groups(repository, group_id),
 FOREIGN KEY (repository, filesystem_id, claim_id)
   REFERENCES managed_storage_claims(repository, filesystem_id, claim_id)
);
CREATE INDEX managed_storage_group_members_by_claim
 ON managed_storage_claim_group_members(repository, filesystem_id, claim_id);
CREATE INDEX IF NOT EXISTS managed_storage_group_members_by_root
 ON managed_storage_claim_group_members(repository, filesystem_id, root_identity, group_id);
CREATE INDEX IF NOT EXISTS managed_storage_group_members_by_root_global
 ON managed_storage_claim_group_members(filesystem_id, root_identity, repository, group_id);
"#;

/// Additive managed-storage producer journal. The durable random producer id
/// and immutable group/plan digests survive restart; a process-local helper id
/// is never written here and no row in this schema grants settlement rights.
const MANAGED_STORAGE_PRODUCER_SCHEMA: &str = r#"
CREATE TABLE managed_storage_preparation_producers (
 repository TEXT NOT NULL,
 producer_id TEXT NOT NULL CHECK (length(producer_id)=64),
 group_id TEXT NOT NULL,
 request_id TEXT NOT NULL,
 lease_id TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK (generation > 0),
 materialization TEXT NOT NULL,
 root_set_digest TEXT NOT NULL,
 pre_inventory_digest TEXT NOT NULL,
 member_manifest_digest TEXT NOT NULL,
 manifest_digest TEXT NOT NULL,
 plan_digest TEXT NOT NULL,
 step_count INTEGER NOT NULL CHECK (step_count BETWEEN 1 AND 32),
 next_step_ordinal INTEGER NOT NULL CHECK (next_step_ordinal BETWEEN 0 AND 32
   AND next_step_ordinal <= step_count),
 state TEXT NOT NULL CHECK (state IN ('active','intent','steps_complete','stopped','uncertain')),
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,
 PRIMARY KEY (repository, producer_id),
 UNIQUE (producer_id),
 UNIQUE (repository, group_id),
 FOREIGN KEY (repository, group_id)
   REFERENCES managed_storage_claim_groups(repository, group_id)
);
CREATE INDEX managed_storage_preparation_producers_by_group
 ON managed_storage_preparation_producers(repository, group_id);
CREATE TABLE managed_storage_preparation_steps (
 repository TEXT NOT NULL,
 producer_id TEXT NOT NULL,
 ordinal INTEGER NOT NULL CHECK (ordinal BETWEEN 0 AND 31),
 name TEXT NOT NULL,
 step_digest TEXT NOT NULL,
 state TEXT NOT NULL CHECK (state IN ('planned','intent','terminal','cleanup_unknown')),
 intent_token TEXT UNIQUE,
 outcome_code TEXT,
 exit_code INTEGER,
 terminal_digest TEXT,
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,
 PRIMARY KEY (repository, producer_id, ordinal),
 FOREIGN KEY (repository, producer_id)
   REFERENCES managed_storage_preparation_producers(repository, producer_id),
 CHECK ((state IN ('planned','intent') AND outcome_code IS NULL AND terminal_digest IS NULL)
     OR (state='terminal' AND outcome_code IS NOT NULL AND terminal_digest IS NOT NULL)
     OR (state='cleanup_unknown' AND outcome_code='cleanup_unknown' AND terminal_digest IS NULL))
);
CREATE INDEX managed_storage_preparation_steps_by_state
 ON managed_storage_preparation_steps(repository, producer_id, state, ordinal);
"#;

/// One immutable proof row binds the Rust-verified joined plan and fresh
/// inventory to the only permitted producer-backed group transition. This is
/// created in the same IMMEDIATE transaction as claim reconciliation.
const MANAGED_STORAGE_PREPARATION_SETTLEMENT_PROOF_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS managed_storage_preparation_settlement_proofs (
 repository TEXT NOT NULL,
 group_id TEXT NOT NULL,
 request_id TEXT NOT NULL,
 lease_id TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK (generation > 0),
 materialization TEXT NOT NULL,
 root_set_digest TEXT NOT NULL,
 pre_inventory_digest TEXT NOT NULL,
 member_manifest_digest TEXT NOT NULL,
 manifest_digest TEXT NOT NULL,
 producer_id TEXT NOT NULL,
 plan_digest TEXT NOT NULL CHECK (length(plan_digest)=64),
 ordered_steps_digest TEXT NOT NULL CHECK (length(ordered_steps_digest)=64),
 inventory_digest TEXT NOT NULL CHECK (length(inventory_digest)=64),
 aggregate_digest TEXT NOT NULL CHECK (length(aggregate_digest)=64),
 created_at INTEGER NOT NULL,
 PRIMARY KEY (repository, group_id),
 UNIQUE (repository, producer_id),
 FOREIGN KEY (repository, group_id)
   REFERENCES managed_storage_claim_groups(repository, group_id),
 FOREIGN KEY (repository, producer_id)
   REFERENCES managed_storage_preparation_producers(repository, producer_id)
);
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_settlement_proof_insert
BEFORE INSERT ON managed_storage_preparation_settlement_proofs
WHEN NOT EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 JOIN managed_storage_claim_groups g
   ON g.repository=p.repository AND g.group_id=p.group_id
 WHERE p.repository=NEW.repository AND p.group_id=NEW.group_id
   AND p.producer_id=NEW.producer_id
   AND p.request_id=NEW.request_id AND p.lease_id=NEW.lease_id
   AND p.generation=NEW.generation
   AND p.materialization=NEW.materialization
   AND p.root_set_digest=NEW.root_set_digest
   AND p.pre_inventory_digest=NEW.pre_inventory_digest
   AND p.member_manifest_digest=NEW.member_manifest_digest
   AND p.manifest_digest=NEW.manifest_digest
   AND p.plan_digest=NEW.plan_digest
   AND p.state='steps_complete' AND p.step_count>0
   AND p.next_step_ordinal=p.step_count
   AND g.repository=NEW.repository AND g.group_id=NEW.group_id
   AND g.request_id=NEW.request_id AND g.lease_id=NEW.lease_id
   AND g.generation=NEW.generation
   AND g.materialization=NEW.materialization
   AND g.root_set_digest=NEW.root_set_digest
   AND g.pre_inventory_digest=NEW.pre_inventory_digest
   AND g.member_manifest_digest=NEW.member_manifest_digest
   AND g.manifest_digest=NEW.manifest_digest
   AND g.state='reserved' AND g.receipt_digest IS NULL
)
BEGIN
 SELECT RAISE(ABORT, 'preparation settlement proof does not match a reserved completed producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_settlement_proof_update
BEFORE UPDATE ON managed_storage_preparation_settlement_proofs
BEGIN
 SELECT RAISE(ABORT, 'preparation settlement proofs are immutable');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_settlement_proof_delete
BEFORE DELETE ON managed_storage_preparation_settlement_proofs
BEGIN
 SELECT RAISE(ABORT, 'preparation settlement proofs are retained');
END;
"#;

/// Immutable first-use evidence for one exact operator-profile/root-set
/// adoption. The JSON receipt carries all per-root measured identities and is
/// retained within the existing per-repository row bound.
const MANAGED_STORAGE_ADOPTION_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS managed_storage_adoptions (
 repository TEXT NOT NULL,
 adoption_id TEXT NOT NULL,
 profile_id TEXT NOT NULL,
 profile_digest TEXT NOT NULL CHECK (length(profile_digest)=64),
 root_set_digest TEXT NOT NULL CHECK (length(root_set_digest)=64),
 inventory_digest TEXT NOT NULL CHECK (length(inventory_digest)=64),
 receipt_json TEXT NOT NULL CHECK (length(receipt_json)<=65536),
 created_at INTEGER NOT NULL,
 PRIMARY KEY (repository, adoption_id)
);
CREATE INDEX IF NOT EXISTS managed_storage_adoptions_by_profile
 ON managed_storage_adoptions(repository, profile_id);
CREATE TRIGGER IF NOT EXISTS managed_storage_adoption_update
BEFORE UPDATE ON managed_storage_adoptions
BEGIN
 SELECT RAISE(ABORT, 'managed-storage adoption records are immutable');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_adoption_delete
BEFORE DELETE ON managed_storage_adoptions
BEGIN
 SELECT RAISE(ABORT, 'managed-storage adoption records are retained');
END;
"#;

/// SQLite triggers make producer-linked root and lease fences effective for
/// older already-open writers. Reapply these idempotently when opening an
/// existing schema-v3 ledger so an earlier unreleased v3 build stays fenced.
const MANAGED_STORAGE_PRODUCER_FENCE_SCHEMA_TEMPLATE: &str = r#"
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_producer_identity_immutable
BEFORE UPDATE OF repository,producer_id,group_id,request_id,lease_id,generation,
 materialization,root_set_digest,pre_inventory_digest,member_manifest_digest,
 manifest_digest,plan_digest,step_count
ON managed_storage_preparation_producers
BEGIN
 SELECT RAISE(ABORT, 'preparation producer identity is immutable');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_producer_state_order
BEFORE UPDATE OF state,next_step_ordinal ON managed_storage_preparation_producers
WHEN NOT (
 (OLD.state='active' AND NEW.state='intent'
   AND NEW.next_step_ordinal=OLD.next_step_ordinal)
 OR (OLD.state='intent' AND NEW.state IN ('active','steps_complete','stopped')
   AND NEW.next_step_ordinal=OLD.next_step_ordinal+1)
 OR (OLD.state='intent' AND NEW.state='uncertain'
   AND NEW.next_step_ordinal=OLD.next_step_ordinal)
)
BEGIN
 SELECT RAISE(ABORT, 'preparation producer state transition is invalid');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_step_identity_immutable
BEFORE UPDATE OF repository,producer_id,ordinal,name,step_digest
ON managed_storage_preparation_steps
BEGIN
 SELECT RAISE(ABORT, 'preparation step identity is immutable');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_step_state_order
BEFORE UPDATE OF state,intent_token,outcome_code,exit_code,terminal_digest
ON managed_storage_preparation_steps
WHEN NOT (
 (OLD.state='planned' AND NEW.state='intent'
   AND OLD.intent_token IS NULL AND NEW.intent_token IS NOT NULL
   AND NEW.outcome_code IS NULL AND NEW.exit_code IS NULL AND NEW.terminal_digest IS NULL)
 OR (OLD.state='intent' AND NEW.state='terminal'
   AND OLD.intent_token IS NOT NULL AND NEW.intent_token=OLD.intent_token
   AND NEW.outcome_code IS NOT NULL AND NEW.terminal_digest IS NOT NULL)
 OR (OLD.state='intent' AND NEW.state='cleanup_unknown'
   AND OLD.intent_token IS NOT NULL AND NEW.intent_token=OLD.intent_token
   AND NEW.outcome_code='cleanup_unknown' AND NEW.exit_code IS NULL
   AND NEW.terminal_digest IS NULL)
)
BEGIN
 SELECT RAISE(ABORT, 'preparation step state transition is invalid');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_producer_retain
BEFORE DELETE ON managed_storage_preparation_producers
BEGIN
 SELECT RAISE(ABORT, 'preparation producer tombstones are retained');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_step_retain
BEFORE DELETE ON managed_storage_preparation_steps
BEGIN
 SELECT RAISE(ABORT, 'preparation step tombstones are retained');
END;

-- Producer-linked roots stay fenced until the owner commits the exact
-- success-only aggregate settlement. These guards live in the additive schema
-- so older already-open writers are fenced by SQLite itself. They protect only
-- the exact linked root set and lease generation; quarantine remains allowed.
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_claim_insert_fence
BEFORE INSERT ON managed_storage_claims
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers producer
 JOIN managed_storage_claim_group_members m
   ON m.repository=producer.repository AND m.group_id=producer.group_id
 WHERE producer.repository=NEW.repository AND m.filesystem_id=NEW.filesystem_id
   AND (m.claim_id=NEW.claim_id OR m.predecessor_claim_id=NEW.claim_id
        OR m.root_identity=NEW.root_identity)
   AND __ROOT_UNRESOLVED_PRODUCERS__
)
BEGIN
 SELECT RAISE(ABORT, 'managed-storage root is held by an unresolved preparation producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_claim_update_fence
BEFORE UPDATE ON managed_storage_claims
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers producer
 JOIN managed_storage_claim_group_members m
   ON m.repository=producer.repository AND m.group_id=producer.group_id
 WHERE ((producer.repository=OLD.repository AND m.filesystem_id=OLD.filesystem_id
         AND (m.claim_id=OLD.claim_id OR m.predecessor_claim_id=OLD.claim_id
              OR m.root_identity=OLD.root_identity)
         AND __ROOT_UNRESOLVED_PRODUCERS__)
    OR (producer.repository=NEW.repository AND m.filesystem_id=NEW.filesystem_id
        AND (m.claim_id=NEW.claim_id OR m.predecessor_claim_id=NEW.claim_id
             OR m.root_identity=NEW.root_identity)
        AND __ROOT_UNRESOLVED_PRODUCERS__))
)
BEGIN
 SELECT RAISE(ABORT, 'managed-storage root is held by an unresolved preparation producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_claim_delete_fence
BEFORE DELETE ON managed_storage_claims
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 JOIN managed_storage_claim_group_members m
   ON m.repository=p.repository AND m.group_id=p.group_id
 WHERE p.repository=OLD.repository AND m.filesystem_id=OLD.filesystem_id
   AND (m.claim_id=OLD.claim_id OR m.predecessor_claim_id=OLD.claim_id
        OR m.root_identity=OLD.root_identity)
)
BEGIN
 SELECT RAISE(ABORT, 'managed-storage root is held by an unresolved preparation producer');
END;

CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_lineage_insert_fence
BEFORE INSERT ON managed_storage_claim_lineage
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers producer
 JOIN managed_storage_claim_group_members m
   ON m.repository=producer.repository AND m.group_id=producer.group_id
 WHERE producer.repository=NEW.repository AND m.filesystem_id=NEW.filesystem_id
   AND (m.claim_id IN (NEW.claim_id,NEW.predecessor_claim_id,NEW.superseded_by_claim_id)
        OR m.predecessor_claim_id IN
           (NEW.claim_id,NEW.predecessor_claim_id,NEW.superseded_by_claim_id))
   AND __ROOT_UNRESOLVED_PRODUCERS__
)
BEGIN
 SELECT RAISE(ABORT, 'managed-storage lineage touches a root held by an unresolved preparation producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_lineage_update_fence
BEFORE UPDATE ON managed_storage_claim_lineage
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers producer
 JOIN managed_storage_claim_group_members m
   ON m.repository=producer.repository AND m.group_id=producer.group_id
 WHERE ((producer.repository=OLD.repository AND m.filesystem_id=OLD.filesystem_id
         AND (m.claim_id IN (OLD.claim_id,OLD.predecessor_claim_id,OLD.superseded_by_claim_id)
              OR m.predecessor_claim_id IN (OLD.claim_id,OLD.predecessor_claim_id,OLD.superseded_by_claim_id))
         AND __ROOT_UNRESOLVED_PRODUCERS__)
    OR (producer.repository=NEW.repository AND m.filesystem_id=NEW.filesystem_id
        AND (m.claim_id IN (NEW.claim_id,NEW.predecessor_claim_id,NEW.superseded_by_claim_id)
             OR m.predecessor_claim_id IN (NEW.claim_id,NEW.predecessor_claim_id,NEW.superseded_by_claim_id))
        AND __ROOT_UNRESOLVED_PRODUCERS__))
)
BEGIN
 SELECT RAISE(ABORT, 'managed-storage lineage touches a root held by an unresolved preparation producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_lineage_delete_fence
BEFORE DELETE ON managed_storage_claim_lineage
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 JOIN managed_storage_claim_group_members m
   ON m.repository=p.repository AND m.group_id=p.group_id
 WHERE p.repository=OLD.repository AND m.filesystem_id=OLD.filesystem_id
   AND (m.claim_id IN (OLD.claim_id,OLD.predecessor_claim_id,OLD.superseded_by_claim_id)
        OR m.predecessor_claim_id IN
           (OLD.claim_id,OLD.predecessor_claim_id,OLD.superseded_by_claim_id))
)
BEGIN
 SELECT RAISE(ABORT, 'managed-storage lineage touches a root held by an unresolved preparation producer');
END;

CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_group_update_fence
BEFORE UPDATE ON managed_storage_claim_groups
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 WHERE p.repository=OLD.repository AND p.group_id=OLD.group_id
)
AND NOT (
 OLD.state='reserved' AND OLD.receipt_digest IS NULL
 AND NEW.state='settled'
 AND NEW.receipt_digest IS NOT NULL
 AND length(NEW.receipt_digest)=64
 AND NEW.receipt_digest NOT GLOB '*[^0-9a-f]*'
 AND NEW.repository IS OLD.repository
 AND NEW.group_id IS OLD.group_id
 AND NEW.request_id IS OLD.request_id
 AND NEW.lease_id IS OLD.lease_id
 AND NEW.generation IS OLD.generation
 AND NEW.materialization IS OLD.materialization
 AND NEW.root_set_digest IS OLD.root_set_digest
 AND NEW.pre_inventory_digest IS OLD.pre_inventory_digest
 AND NEW.member_manifest_digest IS OLD.member_manifest_digest
 AND NEW.manifest_digest IS OLD.manifest_digest
 AND NEW.member_count IS OLD.member_count
 AND NEW.created_at IS OLD.created_at
 AND NEW.updated_at >= OLD.updated_at
 AND (SELECT COUNT(*) FROM managed_storage_claim_group_members m
      WHERE m.repository=OLD.repository AND m.group_id=OLD.group_id)=OLD.member_count
 AND EXISTS (
   SELECT 1 FROM managed_storage_preparation_producers p
   WHERE p.repository=OLD.repository AND p.group_id=OLD.group_id
     AND p.request_id=OLD.request_id AND p.lease_id=OLD.lease_id
     AND p.generation=OLD.generation AND p.materialization=OLD.materialization
     AND p.root_set_digest=OLD.root_set_digest
     AND p.pre_inventory_digest=OLD.pre_inventory_digest
     AND p.member_manifest_digest=OLD.member_manifest_digest
     AND p.manifest_digest=OLD.manifest_digest
     AND p.state='steps_complete' AND p.step_count>0
     AND p.next_step_ordinal=p.step_count
     AND (SELECT COUNT(*) FROM managed_storage_preparation_steps s
          WHERE s.repository=p.repository AND s.producer_id=p.producer_id)=p.step_count
     AND (SELECT MIN(ordinal) FROM managed_storage_preparation_steps s
          WHERE s.repository=p.repository AND s.producer_id=p.producer_id)=0
     AND (SELECT MAX(ordinal) FROM managed_storage_preparation_steps s
          WHERE s.repository=p.repository AND s.producer_id=p.producer_id)=p.step_count-1
     AND NOT EXISTS (
       SELECT 1 FROM managed_storage_preparation_steps s
       WHERE s.repository=p.repository AND s.producer_id=p.producer_id
         AND (s.state IS NOT 'terminal' OR s.outcome_code IS NOT 'succeeded'
              OR s.exit_code IS NOT 0 OR s.intent_token IS NULL
              OR length(s.intent_token) IS NOT 64
              OR s.intent_token GLOB '*[^0-9a-f]*'
              OR s.step_digest IS NULL OR length(s.step_digest) IS NOT 64
              OR s.step_digest GLOB '*[^0-9a-f]*'
              OR s.terminal_digest IS NULL OR length(s.terminal_digest) IS NOT 64
              OR s.terminal_digest GLOB '*[^0-9a-f]*')
     )
     AND EXISTS (
       SELECT 1 FROM managed_storage_preparation_settlement_proofs proof
       WHERE proof.repository=p.repository AND proof.group_id=p.group_id
         AND proof.request_id=p.request_id AND proof.lease_id=p.lease_id
         AND proof.generation=p.generation
         AND proof.materialization=p.materialization
         AND proof.root_set_digest=p.root_set_digest
         AND proof.pre_inventory_digest=p.pre_inventory_digest
         AND proof.member_manifest_digest=p.member_manifest_digest
         AND proof.manifest_digest=p.manifest_digest
         AND proof.producer_id=p.producer_id
         AND proof.plan_digest=p.plan_digest
         AND proof.aggregate_digest=NEW.receipt_digest
     )
 )
)
BEGIN
 SELECT RAISE(ABORT, 'managed-storage group is held by an unresolved preparation producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_group_delete_fence
BEFORE DELETE ON managed_storage_claim_groups
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 WHERE p.repository=OLD.repository AND p.group_id=OLD.group_id
)
BEGIN
 SELECT RAISE(ABORT, 'managed-storage group is held by an unresolved preparation producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_group_member_insert_fence
BEFORE INSERT ON managed_storage_claim_group_members
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers producer
 JOIN managed_storage_claim_group_members held
   ON held.repository=producer.repository AND held.group_id=producer.group_id
 WHERE producer.repository=NEW.repository
   AND (producer.group_id=NEW.group_id
        OR (held.filesystem_id=NEW.filesystem_id
            AND (held.claim_id IN (NEW.claim_id,NEW.predecessor_claim_id)
                 OR held.predecessor_claim_id IN (NEW.claim_id,NEW.predecessor_claim_id)
                 OR held.root_identity=NEW.root_identity)
            AND __ROOT_UNRESOLVED_PRODUCERS__))
)
BEGIN
 SELECT RAISE(ABORT, 'managed-storage group members are held by an unresolved preparation producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_group_member_update_fence
BEFORE UPDATE ON managed_storage_claim_group_members
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 JOIN managed_storage_claim_group_members held
   ON held.repository=p.repository AND held.group_id=p.group_id
 WHERE (p.repository=OLD.repository
        AND (p.group_id=OLD.group_id
             OR (held.filesystem_id=OLD.filesystem_id
                 AND (held.claim_id IN (OLD.claim_id,OLD.predecessor_claim_id)
                      OR held.predecessor_claim_id IN (OLD.claim_id,OLD.predecessor_claim_id)
                      OR held.root_identity=OLD.root_identity))))
    OR (p.repository=NEW.repository
        AND (p.group_id=NEW.group_id
             OR (held.filesystem_id=NEW.filesystem_id
                 AND (held.claim_id IN (NEW.claim_id,NEW.predecessor_claim_id)
                      OR held.predecessor_claim_id IN (NEW.claim_id,NEW.predecessor_claim_id)
                      OR held.root_identity=NEW.root_identity))))
)
BEGIN
 SELECT RAISE(ABORT, 'managed-storage group members are held by an unresolved preparation producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_group_member_delete_fence
BEFORE DELETE ON managed_storage_claim_group_members
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 JOIN managed_storage_claim_group_members held
   ON held.repository=p.repository AND held.group_id=p.group_id
 WHERE p.repository=OLD.repository
   AND (p.group_id=OLD.group_id
        OR (held.filesystem_id=OLD.filesystem_id
            AND (held.claim_id IN (OLD.claim_id,OLD.predecessor_claim_id)
                 OR held.predecessor_claim_id IN (OLD.claim_id,OLD.predecessor_claim_id)
                 OR held.root_identity=OLD.root_identity)))
)
BEGIN
 SELECT RAISE(ABORT, 'managed-storage group members are held by an unresolved preparation producer');
END;

CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_lease_identity_fence
BEFORE UPDATE OF lease_id,request_id,request_digest,repository,worktree_fingerprint,
 run_id,generation,ownership_token,holder_pid,created_at,released_at ON resource_leases
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 WHERE p.lease_id=OLD.lease_id AND p.generation=OLD.generation
)
 AND (NEW.lease_id IS NOT OLD.lease_id OR NEW.request_id IS NOT OLD.request_id
      OR NEW.request_digest IS NOT OLD.request_digest OR NEW.repository IS NOT OLD.repository
      OR NEW.worktree_fingerprint IS NOT OLD.worktree_fingerprint
      OR NEW.run_id IS NOT OLD.run_id OR NEW.generation IS NOT OLD.generation
      OR NEW.ownership_token IS NOT OLD.ownership_token
      OR NEW.holder_pid IS NOT OLD.holder_pid
      OR NEW.created_at IS NOT OLD.created_at
      OR (NEW.released_at IS NOT OLD.released_at AND NOT (
          OLD.state IN ('active','quarantined')
          AND OLD.released_at IS NULL
          AND NEW.state='released'
          AND NEW.released_at IS NOT NULL
          AND NEW.released_at=NEW.updated_at
          AND NOT EXISTS (
              SELECT 1 FROM resource_supervised_capacity_holds h
              WHERE h.lease_id=OLD.lease_id AND h.generation=OLD.generation
          )
          AND NOT (__OLD_UNRESOLVED_PRODUCERS__)
      )))
BEGIN
 SELECT RAISE(ABORT, 'resource lease identity is held by an unresolved preparation producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_lease_state_fence
BEFORE UPDATE OF state ON resource_leases
WHEN NEW.state IS NOT OLD.state
 AND EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 WHERE p.lease_id=OLD.lease_id AND p.generation=OLD.generation
)
 AND NOT (
     (NEW.state='quarantined' AND OLD.state!='released')
     OR (OLD.state IN ('active','quarantined')
         AND OLD.released_at IS NULL
         AND NEW.state='released'
         AND NEW.released_at IS NOT NULL
         AND NEW.released_at=NEW.updated_at
         AND NOT EXISTS (
             SELECT 1 FROM resource_supervised_capacity_holds h
             WHERE h.lease_id=OLD.lease_id AND h.generation=OLD.generation
         )
         AND NOT (__OLD_UNRESOLVED_PRODUCERS__))
 )
BEGIN
 SELECT RAISE(ABORT, 'resource lease cannot leave quarantine-safe states while preparation is unresolved');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_lease_delete_fence
BEFORE DELETE ON resource_leases
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 WHERE p.lease_id=OLD.lease_id AND p.generation=OLD.generation
)
BEGIN
 SELECT RAISE(ABORT, 'resource lease is held by an unresolved preparation producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_allocation_insert_fence
BEFORE INSERT ON resource_allocations
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 JOIN resource_leases l ON l.lease_id=p.lease_id AND l.generation=p.generation
 WHERE l.lease_id=NEW.lease_id
)
BEGIN
 SELECT RAISE(ABORT, 'resource allocations are held by an unresolved preparation producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_allocation_update_fence
BEFORE UPDATE ON resource_allocations
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 JOIN resource_leases l ON l.lease_id=p.lease_id AND l.generation=p.generation
 WHERE l.lease_id=OLD.lease_id OR l.lease_id=NEW.lease_id
)
BEGIN
 SELECT RAISE(ABORT, 'resource allocations are held by an unresolved preparation producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_allocation_delete_fence
BEFORE DELETE ON resource_allocations
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 JOIN resource_leases l ON l.lease_id=p.lease_id AND l.generation=p.generation
 WHERE l.lease_id=OLD.lease_id
)
 AND NOT (
     OLD.kind='capacity'
     AND EXISTS (
         SELECT 1 FROM resource_leases l
         WHERE l.lease_id=OLD.lease_id AND l.state='quarantined'
           AND EXISTS (
               SELECT 1 FROM resource_reap_observations r
               WHERE r.lease_id=l.lease_id AND r.generation=l.generation
           )
           AND NOT EXISTS (
               SELECT 1 FROM resource_supervised_capacity_holds h
               WHERE h.lease_id=l.lease_id AND h.generation=l.generation
           )
           AND NOT (__CURRENT_UNRESOLVED_PRODUCERS__)
     )
 )
BEGIN
 SELECT RAISE(ABORT, 'resource allocations are held by an unresolved preparation producer');
END;
CREATE TRIGGER IF NOT EXISTS managed_storage_preparation_reaper_skip
BEFORE INSERT ON resource_reap_observations
WHEN EXISTS (
 SELECT 1 FROM managed_storage_preparation_producers p
 WHERE p.lease_id=NEW.lease_id
)
 AND NOT EXISTS (
     SELECT 1 FROM resource_leases l
     WHERE l.lease_id=NEW.lease_id AND l.generation=NEW.generation
       AND l.state='quarantined'
       AND NOT EXISTS (
           SELECT 1 FROM resource_supervised_capacity_holds h
           WHERE h.lease_id=l.lease_id AND h.generation=l.generation
       )
       AND NOT (__CURRENT_UNRESOLVED_PRODUCERS__)
 )
BEGIN
 SELECT RAISE(IGNORE);
END;
"#;

/// This expression is shared by Rust admission/reaping and the SQLite
/// old-writer triggers. Its SQL expression arguments are fixed source literals
/// (or fixed bind placeholders), never caller-provided values.
fn managed_storage_unresolved_producer_predicate_sql(
    lease_id_sql: &str,
    generation_sql: &str,
) -> String {
    format!(
        r#"EXISTS (
            SELECT 1 FROM managed_storage_preparation_producers p
            WHERE p.lease_id={lease_id_sql} AND p.generation={generation_sql}
              AND NOT (
                  p.state='steps_complete' AND p.step_count>0
                  AND p.next_step_ordinal=p.step_count
                  AND EXISTS (
                      SELECT 1
                      FROM managed_storage_claim_groups g
                      JOIN managed_storage_preparation_settlement_proofs proof
                        ON proof.repository=g.repository AND proof.group_id=g.group_id
                      WHERE g.repository=p.repository AND g.group_id=p.group_id
                        AND g.request_id=p.request_id AND g.lease_id=p.lease_id
                        AND g.generation=p.generation
                        AND g.materialization=p.materialization
                        AND g.root_set_digest=p.root_set_digest
                        AND g.pre_inventory_digest=p.pre_inventory_digest
                        AND g.member_manifest_digest=p.member_manifest_digest
                        AND g.manifest_digest=p.manifest_digest AND g.state='settled'
                        AND g.receipt_digest=proof.aggregate_digest
                        AND proof.repository=p.repository AND proof.group_id=p.group_id
                        AND proof.request_id=p.request_id AND proof.lease_id=p.lease_id
                        AND proof.generation=p.generation
                        AND proof.materialization=p.materialization
                        AND proof.root_set_digest=p.root_set_digest
                        AND proof.pre_inventory_digest=p.pre_inventory_digest
                        AND proof.member_manifest_digest=p.member_manifest_digest
                        AND proof.manifest_digest=p.manifest_digest
                        AND proof.producer_id=p.producer_id
                        AND proof.plan_digest=p.plan_digest
                  )
              )
        )"#
    )
}

fn managed_storage_producer_fence_schema() -> String {
    let unresolved_old =
        managed_storage_unresolved_producer_predicate_sql("OLD.lease_id", "OLD.generation");
    let unresolved_current =
        managed_storage_unresolved_producer_predicate_sql("l.lease_id", "l.generation");
    let unresolved_root = managed_storage_unresolved_producer_predicate_sql(
        "producer.lease_id",
        "producer.generation",
    );
    MANAGED_STORAGE_PRODUCER_FENCE_SCHEMA_TEMPLATE
        .replace("__OLD_UNRESOLVED_PRODUCERS__", &unresolved_old)
        .replace("__CURRENT_UNRESOLVED_PRODUCERS__", &unresolved_current)
        .replace("__ROOT_UNRESOLVED_PRODUCERS__", &unresolved_root)
}

// Replace the v3 guards only inside the explicit v3-to-v4 activation transaction, so every later v4 open is status-only.
const MANAGED_STORAGE_PREPARATION_SETTLEMENT_TRIGGER_DROP: &str = r#"
DROP TRIGGER IF EXISTS managed_storage_preparation_claim_insert_fence;
DROP TRIGGER IF EXISTS managed_storage_preparation_claim_update_fence;
DROP TRIGGER IF EXISTS managed_storage_preparation_lineage_insert_fence;
DROP TRIGGER IF EXISTS managed_storage_preparation_lineage_update_fence;
DROP TRIGGER IF EXISTS managed_storage_preparation_group_member_insert_fence;
DROP TRIGGER IF EXISTS managed_storage_preparation_group_update_fence;
DROP TRIGGER IF EXISTS managed_storage_preparation_lease_identity_fence;
DROP TRIGGER IF EXISTS managed_storage_preparation_lease_state_fence;
DROP TRIGGER IF EXISTS managed_storage_preparation_allocation_delete_fence;
DROP TRIGGER IF EXISTS managed_storage_preparation_reaper_skip;
"#;

const SUPERVISED_HOLD_MIGRATION: &str = r#"
CREATE TABLE resource_supervised_capacity_holds (
 lease_id TEXT NOT NULL REFERENCES resource_leases(lease_id),
 generation INTEGER NOT NULL,
 created_at INTEGER NOT NULL,
 PRIMARY KEY (lease_id, generation)
);

CREATE TRIGGER resource_supervised_hold_capacity_insert
BEFORE INSERT ON resource_allocations
WHEN NEW.kind='capacity'
 AND EXISTS (
     SELECT 1
     FROM resource_supervised_capacity_holds h
     JOIN resource_leases hl ON hl.lease_id=h.lease_id AND hl.generation=h.generation
     JOIN resource_allocations held ON held.lease_id=h.lease_id
          AND held.kind='capacity' AND held.value=NEW.value
     WHERE hl.state IN ('active','quarantined')
 )
 AND COALESCE((
     SELECT SUM(COALESCE(a.units,0))
     FROM resource_allocations a
     JOIN resource_leases l ON l.lease_id=a.lease_id
     WHERE a.kind='capacity' AND a.value=NEW.value
       AND l.state IN ('active','quarantined')
 ),0) + COALESCE(NEW.units,0) > MIN(
     COALESCE(NEW.capacity_limit,0),
     COALESCE((
         SELECT MIN(a.capacity_limit)
         FROM resource_allocations a
         JOIN resource_leases l ON l.lease_id=a.lease_id
         WHERE a.kind='capacity' AND a.value=NEW.value
           AND l.state IN ('active','quarantined')
     ),COALESCE(NEW.capacity_limit,0))
 )
BEGIN
 SELECT RAISE(ABORT, 'supervised capacity hold exceeds pool limit');
END;

CREATE TRIGGER resource_supervised_hold_capacity_delete
BEFORE DELETE ON resource_allocations
WHEN EXISTS (
 SELECT 1 FROM resource_supervised_capacity_holds h
 JOIN resource_leases l ON l.lease_id=h.lease_id AND l.generation=h.generation
 WHERE h.lease_id=OLD.lease_id
)
BEGIN
 SELECT RAISE(ABORT, 'supervised capacity hold requires verified release or reconciliation');
END;

CREATE TRIGGER resource_supervised_hold_capacity_update
BEFORE UPDATE ON resource_allocations
WHEN EXISTS (
 SELECT 1 FROM resource_supervised_capacity_holds h
 JOIN resource_leases l ON l.lease_id=h.lease_id AND l.generation=h.generation
 WHERE h.lease_id IN (OLD.lease_id,NEW.lease_id)
)
BEGIN
 SELECT RAISE(ABORT, 'supervised capacity hold requires verified release or reconciliation');
END;

CREATE TRIGGER resource_supervised_hold_lease_release
BEFORE UPDATE OF state ON resource_leases
WHEN NEW.state='released' AND OLD.state!='released'
 AND EXISTS (
     SELECT 1 FROM resource_supervised_capacity_holds h
     WHERE h.lease_id=OLD.lease_id AND h.generation=OLD.generation
 )
BEGIN
 SELECT RAISE(ABORT, 'supervised capacity hold requires verified release or reconciliation');
END;

CREATE TRIGGER resource_supervised_hold_lease_delete
BEFORE DELETE ON resource_leases
WHEN EXISTS (
 SELECT 1 FROM resource_supervised_capacity_holds h
 WHERE h.lease_id=OLD.lease_id AND h.generation=OLD.generation
)
BEGIN
 SELECT RAISE(ABORT, 'supervised capacity hold requires verified release or reconciliation');
END;
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

#[allow(
    dead_code,
    reason = "managed preparation cleanup consumer remains disabled pending ownership qualification"
)]
impl VerifiedManagedStorageRemoval {
    #[expect(
        clippy::too_many_arguments,
        reason = "removal proof binds independent authority and identity fields explicitly"
    )]
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

/// Read-only projection of dead-holder capacity reconciliation. The entries
/// describe what a fresh reap could do, not a cleanup grant or completed work.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostResourceReapPreview {
    pub schema: &'static str,
    pub evaluated_at_ms: i64,
    pub would_reclaim_capacity_units: u32,
    /// `state` is the projected state after capacity reconciliation. Named
    /// allocations are retained; no process or container is signalled.
    pub candidates: Vec<HostResourceReapLease>,
    pub not_reaped: Vec<HostResourceNotReapedLease>,
}

/// The legacy reap counters plus diagnostics for candidate leases that were
/// kept because the holder's absence could not be proven.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostResourceReapReportWithDiagnostics {
    #[serde(flatten)]
    pub report: HostResourceReapReport,
    pub not_reaped: Vec<HostResourceNotReapedLease>,
}

/// A quarantined lease that the reaper safely left untouched.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostResourceNotReapedLease {
    pub lease_id: String,
    pub reason: HostResourceNotReapedReason,
}

/// Why a quarantined lease candidate was not reclaimed.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostResourceNotReapedReason {
    /// The holder process could not be proven absent.
    NotProvenGone,
}

impl HostResourceNotReapedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotProvenGone => "not_proven_gone",
        }
    }
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
    #[error("cannot confirm supervised child exit: {detail}; lease {lease_id:?} {lease_state}")]
    TerminationUnconfirmed {
        lease_id: String,
        lease_state: String,
        detail: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HostResourceDatabaseIdentity {
    /// Absolute path supplied by the caller, captured before SQLite opens it.
    requested_path: PathBuf,
    canonical_path: PathBuf,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl HostResourceDatabaseIdentity {
    fn absolute_path(path: &Path) -> Result<PathBuf, HostResourceError> {
        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            Ok(std::env::current_dir()
                .map_err(|source| HostResourceError::Io {
                    path: PathBuf::from("."),
                    source,
                })?
                .join(path))
        }
    }

    fn capture_if_present(path: &Path) -> Result<Option<Self>, HostResourceError> {
        match Self::capture(path) {
            Ok(identity) => Ok(Some(identity)),
            Err(HostResourceError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    fn capture(path: &Path) -> Result<Self, HostResourceError> {
        let requested_path = Self::absolute_path(path)?;
        let canonical_path =
            requested_path
                .canonicalize()
                .map_err(|source| HostResourceError::Io {
                    path: requested_path.clone(),
                    source,
                })?;
        let metadata =
            std::fs::metadata(&canonical_path).map_err(|source| HostResourceError::Io {
                path: canonical_path.clone(),
                source,
            })?;
        if !metadata.is_file() {
            return Err(HostResourceError::InvalidRequest(
                "host resource database path is not a regular file".into(),
            ));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Ok(Self {
                requested_path,
                canonical_path,
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            // Other targets are bound to the canonical path only; this does
            // not claim to detect replacement at the same path.
            Ok(Self {
                requested_path,
                canonical_path,
            })
        }
    }

    fn ensure_current_path_identity(&self) -> Result<(), HostResourceError> {
        let current = Self::capture(&self.requested_path)?;
        if current != *self {
            return Err(HostResourceError::InvalidRequest(
                "host resource database path no longer identifies the database opened for managed-storage activation".into(),
            ));
        }
        Ok(())
    }
}

pub struct HostResourceCoordinator {
    conn: Connection,
    path: PathBuf,
    database_identity: Option<HostResourceDatabaseIdentity>,
}

/// A typed acknowledgement for the host-wide managed-storage floor change.
/// The default coordinator open deliberately never creates this capability.
#[allow(
    dead_code,
    reason = "managed-storage schema activation awaits coordinated host-wide writer quiescence"
)]
pub(crate) struct ManagedStorageSchemaActivationConfirmation {
    database_identity: HostResourceDatabaseIdentity,
}

#[allow(
    dead_code,
    reason = "managed-storage schema activation awaits coordinated host-wide writer quiescence"
)]
impl ManagedStorageSchemaActivationConfirmation {
    const ACKNOWLEDGEMENT: &'static str = "ACTIVATE_SHARED_HOST_MANAGED_STORAGE_SCHEMA_V4";

    pub(crate) fn explicitly_confirmed(
        database_path: &Path,
        acknowledgement: &str,
    ) -> Result<Self, HostResourceError> {
        if acknowledgement != Self::ACKNOWLEDGEMENT {
            return Err(HostResourceError::InvalidRequest(
                "managed-storage schema activation requires the exact host-wide confirmation"
                    .into(),
            ));
        }
        Ok(Self {
            database_identity: HostResourceDatabaseIdentity::capture(database_path)?,
        })
    }
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
        // Anchor relative paths once, then pass that exact absolute path to
        // SQLite and every identity check. Resolving the relative path again
        // after open would let a process-wide cwd change bind the token to a
        // different file than the connection opened.
        let open_path = HostResourceDatabaseIdentity::absolute_path(path)?;
        if let Some(parent) = open_path.parent() {
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
        let identity_before_open = HostResourceDatabaseIdentity::capture_if_present(&open_path)?;
        let mut conn = Connection::open(&open_path)?;
        let database_identity = HostResourceDatabaseIdentity::capture(&open_path)?;
        if identity_before_open
            .as_ref()
            .is_some_and(|identity| identity != &database_identity)
        {
            return Err(HostResourceError::InvalidRequest(
                "host resource database path changed while SQLite was opening it".into(),
            ));
        }
        crate::host_state::protect_host_state_path(&open_path, false).map_err(|source| {
            HostResourceError::Io {
                path: open_path.clone(),
                source,
            }
        })?;
        conn.busy_timeout(busy_timeout)?;
        preflight_existing_store_versions(&conn)?;
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate_writable_schema(&mut conn)?;
        validate_schema(&conn)?;
        validate_managed_storage_schema_supported(&conn)?;
        Ok(Self {
            conn,
            path: path.into(),
            database_identity: Some(database_identity),
        })
    }

    /// Open without touching durable state. A missing registry is represented
    /// by an initialized in-memory database, so first-use planning stays pure.
    pub fn open_read_only_default() -> Result<Self, HostResourceError> {
        Self::open_read_only(&default_host_resource_db_path()?)
    }

    pub fn open_read_only(path: &Path) -> Result<Self, HostResourceError> {
        let open_path = HostResourceDatabaseIdentity::absolute_path(path)?;
        if !open_path.exists() {
            let conn = Connection::open_in_memory()?;
            conn.execute_batch(SCHEMA)?;
            return Ok(Self {
                conn,
                path: path.into(),
                database_identity: None,
            });
        }
        let identity_before_open = HostResourceDatabaseIdentity::capture_if_present(&open_path)?
            .ok_or_else(|| {
                HostResourceError::InvalidRequest(
                    "host resource database disappeared before read-only open".into(),
                )
            })?;
        let conn = Connection::open_with_flags(&open_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let database_identity = HostResourceDatabaseIdentity::capture(&open_path)?;
        if identity_before_open != database_identity {
            return Err(HostResourceError::InvalidRequest(
                "host resource database path changed while SQLite was opening it".into(),
            ));
        }
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        validate_schema(&conn)?;
        Ok(Self {
            conn,
            path: path.into(),
            database_identity: Some(database_identity),
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.path
    }

    /// Explicitly advance the managed-storage floor on this already-open host
    /// database. Ordinary coordinator opens preserve the shared schema-v1
    /// floor so older repository worktrees remain usable until every writer is
    /// coordinated for activation.
    ///
    /// The stored canonical path and, on Unix, device/inode are checked at
    /// several transaction boundaries to detect observed alias drift or file
    /// replacement. They cannot atomically pin SQLite's internal file handle
    /// against a hostile replacement between checks. Callers must coordinate
    /// host-wide writers and treat these checks as stale-path detection, not as
    /// protection from an uncooperative path replacer.
    #[allow(
        dead_code,
        reason = "managed-storage schema activation awaits coordinated host-wide writer quiescence"
    )]
    pub(crate) fn activate_managed_storage_schema(
        &mut self,
        confirmation: ManagedStorageSchemaActivationConfirmation,
    ) -> Result<(), HostResourceError> {
        let opened_identity = self.database_identity.as_ref().ok_or_else(|| {
            HostResourceError::InvalidRequest(
                "managed-storage activation requires an opened file-backed host database".into(),
            )
        })?;
        if opened_identity != &confirmation.database_identity {
            return Err(HostResourceError::InvalidRequest(
                "managed-storage schema confirmation is bound to a different opened host database"
                    .into(),
            ));
        }
        opened_identity.ensure_current_path_identity()?;

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        opened_identity.ensure_current_path_identity()?;
        preflight_existing_store_versions_through(&tx, MANAGED_STORAGE_SCHEMA_VERSION)?;
        opened_identity.ensure_current_path_identity()?;
        migrate_managed_storage_schema(&tx)?;
        validate_schema(&tx)?;
        validate_managed_storage_schema(&tx)?;
        opened_identity.ensure_current_path_identity()?;
        tx.commit()?;
        Ok(())
    }

    /// Hold the exact preparation-cache exclusive lease while managing one
    /// finite materialization directory. The caller supplies the resolved
    /// preparation-cache root from this broker's storage context; this API does
    /// not constrain arbitrary child-process writes or prove child quiescence.
    #[allow(
        dead_code,
        reason = "managed preparation consumer remains disabled until its ownership fences are qualified"
    )]
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

    /// Begin root-set inventory using the same exact preparation-cache lease
    /// and per-root scanner as the existing single-root owner. This does not
    /// resolve profile selectors or authorize materialization growth.
    #[allow(
        dead_code,
        reason = "managed profile callers are staged separately from root-set inventory"
    )]
    pub(crate) fn begin_managed_storage_root_set_owner<'a>(
        &'a mut self,
        grant: HostResourceGrant,
        repository: &str,
        preparation_cache_root: &Path,
        binding: &ManagedStorageRootSetBinding,
        ttl_seconds: u64,
    ) -> Result<ManagedStorageOwner<'a>, HostResourceError> {
        let materialization = binding.first_materialization()?;
        let mut owner = ManagedStorageOwner::new(
            self,
            grant,
            repository,
            preparation_cache_root,
            materialization,
            ttl_seconds,
        )?;
        // Validate the complete binding against the same owner grant before
        // handing the owner to a caller. The returned scan is intentionally
        // discarded; scanning begins only when the caller explicitly polls.
        drop(owner.begin_root_set_inventory_scan(binding)?);
        Ok(owner)
    }

    /// Read one exact durable claim so a restarted owner can distinguish an
    /// outstanding reservation from measured, uncertain, or released storage.
    #[allow(
        dead_code,
        reason = "managed preparation consumer remains disabled until its ownership fences are qualified"
    )]
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
    #[allow(
        dead_code,
        reason = "managed preparation consumer remains disabled until its ownership fences are qualified"
    )]
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
    #[allow(
        dead_code,
        reason = "managed preparation cleanup consumer remains disabled pending ownership qualification"
    )]
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
    #[allow(
        dead_code,
        reason = "managed preparation cleanup consumer remains disabled pending ownership qualification"
    )]
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
        self.acquire_inner(request, false)
    }

    fn acquire_inner(
        &mut self,
        request: &HostResourceRequest,
        supervised: bool,
    ) -> Result<HostResourceGrant, HostResourceError> {
        validate_request(request)?;
        let digest = if supervised {
            supervised_request_digest(request)?
        } else {
            request_digest(request)?
        };
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
            if supervised {
                return Err(HostResourceError::RequestNotActive {
                    request_id: request.request_id.clone(),
                    state: "already used by a supervised run".into(),
                });
            }
            if state != "active" { return Err(HostResourceError::RequestNotActive { request_id: request.request_id.clone(), state }); }
            let lease = load_lease(&tx, "request_id", &request.request_id)?.ok_or_else(|| HostResourceError::LeaseNotFound(request.request_id.clone()))?;
            tx.commit()?;
            return Ok(HostResourceGrant { lease, ownership_token: token });
        }
        let occupied = load_occupied(&tx)?;
        let (allocations, conflicts) = plan_allocations(request, &occupied)?;
        if !conflicts.is_empty() {
            let code = aggregate_conflict_code(&conflicts).to_string();
            if code == ORPHANED_HOLDER_CONFLICT_CODE {
                quarantine_conflicting_orphaned_holders(&tx, now, &conflicts, &occupied)?;
                // This commits the existing safe cleanup sweep plus quarantine
                // for the exact dead owners behind the non-waitable result.
                // Named allocations remain, and rejected-bundle allocations
                // are not inserted before this branch.
                tx.commit()?;
            }
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
        if supervised {
            tx.execute(
                "INSERT INTO resource_supervised_capacity_holds(lease_id,generation,created_at) VALUES (?1,?2,?3)",
                params![lease_id, generation, now],
            )?;
        }
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
        self.recover_request_inner(request, false)
    }

    // Keep supervised recovery internal until a durable service journal and
    // recovery lock bind the caller. An exact request is identity, not a public
    // capability to retrieve another controller's private ownership token.
    fn recover_request_inner(
        &self,
        request: &HostResourceRequest,
        supervised: bool,
    ) -> Result<Option<HostResourceGrant>, HostResourceError> {
        validate_request(request)?;
        let digest = if supervised {
            supervised_request_digest(request)?
        } else {
            request_digest(request)?
        };
        // Read identity, state, allocations and hold from one SQLite snapshot.
        // Reconciliation may settle the grant concurrently; recovery must not
        // combine a pre-settlement token with post-settlement hold observations.
        let tx = self.conn.unchecked_transaction()?;
        let found = tx
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
        let lease = load_lease(&tx, "request_id", &request.request_id)?
            .ok_or_else(|| HostResourceError::LeaseNotFound(request.request_id.clone()))?;
        if supervised
            && lease.state != HostLeaseState::Released
            && !has_supervised_capacity_hold(&tx, &lease.lease_id, lease.generation)?
        {
            return Err(HostResourceError::InvalidRequest(
                "supervised recovery requires its generation-bound hold".into(),
            ));
        }
        tx.commit()?;
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
        on_conflict: F,
    ) -> Result<HostResourceGrant, HostResourceError>
    where
        F: FnMut(&str),
    {
        self.acquire_with_wait_inner(request, wait, false, on_conflict)
    }

    fn acquire_with_wait_inner<F>(
        &mut self,
        request: &HostResourceRequest,
        wait: std::time::Duration,
        supervised: bool,
        mut on_conflict: F,
    ) -> Result<HostResourceGrant, HostResourceError>
    where
        F: FnMut(&str),
    {
        let deadline = std::time::Instant::now() + wait;
        loop {
            match self.acquire_inner(request, supervised) {
                Ok(grant) => return Ok(grant),
                Err(HostResourceError::Conflict {
                    code,
                    message,
                    conflicts: _,
                }) if code == "resource_contention" && std::time::Instant::now() < deadline => {
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
        if has_supervised_capacity_hold(&tx, lease_id, generation)? {
            return Err(HostResourceError::InvalidRequest(
                "supervised resource hold requires verified terminal release or reconciliation"
                    .into(),
            ));
        }
        tx.execute("UPDATE resource_leases SET state='released',released_at=?2,updated_at=?2 WHERE lease_id=?1 AND state!='released'", params![lease_id,now])?;
        let lease = load_lease(&tx, "lease_id", lease_id)?
            .ok_or_else(|| HostResourceError::LeaseNotFound(lease_id.into()))?;
        tx.commit()?;
        Ok(lease)
    }

    fn release_supervised_verified(
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
        if !has_supervised_capacity_hold(&tx, lease_id, generation)? {
            return Err(HostResourceError::InvalidRequest(
                "supervised terminal release requires its generation-bound hold".into(),
            ));
        }
        tx.execute(
            "DELETE FROM resource_supervised_capacity_holds WHERE lease_id=?1 AND generation=?2",
            params![lease_id, generation as i64],
        )?;
        tx.execute(
            "UPDATE resource_leases SET state='released',released_at=?2,updated_at=?2 \
             WHERE lease_id=?1 AND state!='released'",
            params![lease_id, now],
        )?;
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

    /// Preview the existing capacity reaper without changing lease states,
    /// allocations or observations. Apply never consumes this projection:
    /// it independently rechecks holder absence and generation-bound fences.
    pub fn preview_dead_holders(&self) -> Result<HostResourceReapPreview, HostResourceError> {
        let now = now_ms();
        let tx = self.conn.unchecked_transaction()?;
        let mut preview = HostResourceReapPreview {
            schema: "aethyme.host-resource-reap-preview/v1",
            evaluated_at_ms: now,
            would_reclaim_capacity_units: 0,
            candidates: Vec::new(),
            not_reaped: Vec::new(),
        };
        let any_retained_lease: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM resource_leases WHERE state!='released')",
            [],
            |row| row.get(0),
        )?;
        if !any_retained_lease {
            // First-use read-only opens use a v1 in-memory registry. There is
            // nothing to reconcile and no need to initialize newer tables.
            tx.commit()?;
            return Ok(preview);
        }
        if validate_schema(&tx)? != HOST_RESOURCE_STORE_SCHEMA_VERSION {
            return Err(HostResourceError::InvalidRequest(
                "reap preview needs a coordinated writable upgrade of this legacy registry; no changes made".into(),
            ));
        }
        // Keep existing managed-storage floors and query only their known
        // tables. Older producer records without immutable settlement proof
        // remain fenced; preview never activates a schema.
        validate_managed_storage_schema_supported(&tx)?;
        for candidate in pending_reap_candidates(&tx)? {
            if !holder_process_is_gone(candidate.holder_pid) {
                if candidate.quarantined || candidate.expires_at <= now {
                    preview.not_reaped.push(HostResourceNotReapedLease {
                        lease_id: candidate.lease_id,
                        reason: HostResourceNotReapedReason::NotProvenGone,
                    });
                }
                continue;
            }
            if candidate.unresolved_preparation {
                // The real reaper's insertion trigger keeps this fence. Its
                // projection must not promise capacity that cannot be freed.
                continue;
            }
            let (capacity_units, named_allocations): (u32, u64) = tx.query_row(
                "SELECT COALESCE(SUM(CASE WHEN kind='capacity' THEN units ELSE 0 END), 0),
                        COUNT(CASE WHEN kind!='capacity' THEN 1 END)
                 FROM resource_allocations WHERE lease_id=?1",
                [&candidate.lease_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            preview.would_reclaim_capacity_units = preview
                .would_reclaim_capacity_units
                .saturating_add(capacity_units);
            preview.candidates.push(HostResourceReapLease {
                lease_id: candidate.lease_id,
                generation: candidate.generation,
                holder_pid: candidate.holder_pid as u32,
                capacity_units,
                state: if named_allocations == 0 {
                    HostLeaseState::Released
                } else {
                    HostLeaseState::Quarantined
                },
            });
        }
        tx.commit()?;
        Ok(preview)
    }

    /// Reclaim capacity held by quarantined leases whose holder process is
    /// provably gone. This is an explicit independent trigger: a scheduler or
    /// operator can run it without waiting for another acquisition to arrive.
    /// Named allocations stay quarantined until exact cleanup is reviewed.
    pub fn reap_dead_holders(&mut self) -> Result<HostResourceReapReport, HostResourceError> {
        self.reap_dead_holders_with_diagnostics()
            .map(|report| report.report)
    }

    /// Reap dead holders and report quarantined candidates that were retained
    /// because their holder process was not proven gone.
    pub fn reap_dead_holders_with_diagnostics(
        &mut self,
    ) -> Result<HostResourceReapReportWithDiagnostics, HostResourceError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let report = quarantine_reclaimable_with_diagnostics(&tx, now)?;
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
    ///
    /// On Unix, requested cleanup runs only after the command's original
    /// process group is observed absent. This bounded check cannot see a
    /// descendant that deliberately leaves that group, for example with
    /// `setsid` or `setpgid`. On other platforms, group quiescence cannot be
    /// proven and the lease is retained in quarantine.
    #[allow(clippy::too_many_arguments)]
    pub fn run_supervised_with_environment<F, P>(
        &mut self,
        request: &HostResourceRequest,
        wait: std::time::Duration,
        command: &[String],
        cleanup_command: Option<&str>,
        cwd: &Path,
        prepare_environment: P,
        event: F,
    ) -> Result<HostResourceRunReport, HostResourceRunError>
    where
        F: FnMut(&str),
        P: FnMut(&HostResourceGrant) -> Result<BTreeMap<String, String>, std::io::Error>,
    {
        self.run_supervised_with_environment_observed(
            request,
            wait,
            command,
            cleanup_command,
            cwd,
            prepare_environment,
            event,
            |_, _| {},
        )
    }

    /// CLI-facing execution path that reports completed wall-clock phases
    /// without changing the public `HostResourceRunReport` Rust API.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn run_supervised_with_environment_observed<F, P, O>(
        &mut self,
        request: &HostResourceRequest,
        wait: std::time::Duration,
        command: &[String],
        cleanup_command: Option<&str>,
        cwd: &Path,
        mut prepare_environment: P,
        mut event: F,
        mut observe_phase: O,
    ) -> Result<HostResourceRunReport, HostResourceRunError>
    where
        F: FnMut(&str),
        P: FnMut(&HostResourceGrant) -> Result<BTreeMap<String, String>, std::io::Error>,
        O: FnMut(&'static str, u128),
    {
        if command.is_empty() {
            return Err(HostResourceRunError::Resource(
                HostResourceError::InvalidRequest("supervised command must not be empty".into()),
            ));
        }
        let acquire_started = std::time::Instant::now();
        let mut last_wait_notice = None;
        let grant_result = self.acquire_with_wait_inner(request, wait, true, |message| {
            let emit = last_wait_notice.is_none_or(|last: std::time::Instant| {
                last.elapsed() >= std::time::Duration::from_secs(10)
            });
            if emit {
                event(&format!("waiting: {message}"));
                last_wait_notice = Some(std::time::Instant::now());
            }
        });
        let waited_ms = acquire_started.elapsed().as_millis();
        observe_phase("resource_acquisition", waited_ms);
        let mut grant = grant_result?;
        event(&format!(
            "granted lease {} generation {} after {}ms",
            grant.lease.lease_id, grant.lease.generation, waited_ms
        ));

        let environment_started = std::time::Instant::now();
        let environment_result = prepare_environment(&grant);
        observe_phase(
            "environment_preparation",
            environment_started.elapsed().as_millis(),
        );
        let environment = match environment_result {
            Ok(environment) => environment,
            Err(source) => {
                crate::warn_unrecorded(
                    "quarantine the host resource lease",
                    quarantine_run_lease_observed(self, &grant, &mut observe_phase),
                );
                return Err(HostResourceRunError::Environment(source));
            }
        };

        let child_spawn_started = std::time::Instant::now();
        let child_result =
            spawn_resource_process(command, cwd, &grant, &environment).map_err(|source| {
                HostResourceRunError::Spawn {
                    program: command[0].clone(),
                    source,
                }
            });
        observe_phase("child_spawn", child_spawn_started.elapsed().as_millis());
        let mut child = match child_result {
            Ok(child) => child,
            Err(error) => {
                crate::warn_unrecorded(
                    "quarantine the host resource lease",
                    quarantine_run_lease_observed(self, &grant, &mut observe_phase),
                );
                return Err(error);
            }
        };
        let child_execution_started = std::time::Instant::now();
        let child_completion_result = supervise_process(
            self,
            &mut grant,
            request.ttl_seconds,
            &mut child,
            &mut event,
            &mut observe_phase,
        );
        observe_phase(
            "child_execution",
            child_execution_started.elapsed().as_millis(),
        );
        let child_completion = child_completion_result?;
        let child_signal = exit_signal(&child_completion.status);
        let child_exit_code = portable_exit_code(&child_completion.status);

        let mut cleanup_exit_code = None;
        let child_group_absent = child_completion.process_group == ProcessGroupObservation::Absent;
        let mut lifecycle_safe = child_group_absent;
        if !child_group_absent {
            event(&format!(
                "original supervised process group {} after leader exit; resource lease will be quarantined",
                child_completion.process_group.description()
            ));
        }
        if let Some(cleanup) = cleanup_command {
            if child_group_absent {
                let cleanup_started = std::time::Instant::now();
                event("running exact cleanup command");
                let cleanup_argv = vec!["sh".to_string(), "-c".to_string(), cleanup.to_string()];
                let cleanup_child =
                    spawn_resource_process(&cleanup_argv, cwd, &grant, &environment).map_err(
                        |source| HostResourceRunError::Spawn {
                            program: "sh".into(),
                            source,
                        },
                    );
                let mut cleanup_child = match cleanup_child {
                    Ok(child) => child,
                    Err(error) => {
                        observe_phase("cleanup_execution", cleanup_started.elapsed().as_millis());
                        crate::warn_unrecorded(
                            "quarantine the host resource lease",
                            quarantine_run_lease_observed(self, &grant, &mut observe_phase),
                        );
                        return Err(error);
                    }
                };
                let cleanup_completion_result = supervise_process(
                    self,
                    &mut grant,
                    request.ttl_seconds,
                    &mut cleanup_child,
                    &mut event,
                    &mut observe_phase,
                );
                observe_phase("cleanup_execution", cleanup_started.elapsed().as_millis());
                let cleanup_completion = cleanup_completion_result?;
                cleanup_exit_code = Some(portable_exit_code(&cleanup_completion.status));
                let cleanup_group_absent =
                    cleanup_completion.process_group == ProcessGroupObservation::Absent;
                lifecycle_safe = cleanup_completion.status.success() && cleanup_group_absent;
                if !cleanup_group_absent {
                    event(&format!(
                        "cleanup process group {} after leader exit; resource lease will be quarantined",
                        cleanup_completion.process_group.description()
                    ));
                }
            } else {
                event(
                    "skipping exact cleanup command because the original process group may still contain writers",
                );
            }
        }

        let authority_lost = grant.lease.state != HostLeaseState::Active;
        let finalization_started = std::time::Instant::now();
        let final_lease_result = if lifecycle_safe && !authority_lost {
            self.release_supervised_verified(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
        } else {
            self.quarantine(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
        };
        observe_phase(
            "lease_finalization",
            finalization_started.elapsed().as_millis(),
        );
        let final_lease = final_lease_result?;
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
        tx.execute(
            "DELETE FROM resource_supervised_capacity_holds WHERE lease_id=?1 AND generation=?2",
            params![lease_id, confirmed as i64],
        )?;
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
            let Some(mut lease) = load_lease(&self.conn, "lease_id", &id)? else {
                continue;
            };
            if lease.state == HostLeaseState::Active && lease.expires_at <= now_ms() {
                lease.state = HostLeaseState::Quarantined;
            }
            leases.push(lease);
        }
        Ok(leases)
    }

    /// Read only leases that are quarantined now, for blocker advice.
    ///
    /// The general inventory remains `list`; this narrower path uses the
    /// state index instead of walking released history in generation order.
    pub(crate) fn quarantined_leases(&self) -> Result<Vec<HostResourceLease>, HostResourceError> {
        self.quarantined_leases_at(now_ms())
    }

    fn quarantined_leases_at(&self, now: i64) -> Result<Vec<HostResourceLease>, HostResourceError> {
        let mut stmt = self.conn.prepare(
            "SELECT lease_id FROM resource_leases
             WHERE state IN ('active','quarantined')
             ORDER BY generation",
        )?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut leases = Vec::new();
        for id in ids {
            let Some(mut lease) = load_lease(&self.conn, "lease_id", &id)? else {
                continue;
            };
            match lease.state {
                HostLeaseState::Quarantined => leases.push(lease),
                HostLeaseState::Active if lease.expires_at <= now => {
                    lease.state = HostLeaseState::Quarantined;
                    leases.push(lease);
                }
                HostLeaseState::Active | HostLeaseState::Released => {}
            }
        }
        Ok(leases)
    }

    /// Read active leases with a caller-owned time and count bound. `None`
    /// means the inventory is incomplete and must not be interpreted as an
    /// empty or complete lease set.
    pub(crate) fn list_until(
        &self,
        include_released: bool,
        entry_budget: usize,
        mut expired: impl FnMut() -> bool,
    ) -> Result<Option<Vec<HostResourceLease>>, HostResourceError> {
        let sql = if include_released {
            "SELECT lease_id FROM resource_leases ORDER BY generation"
        } else {
            "SELECT lease_id FROM resource_leases WHERE state!='released' ORDER BY generation"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query([])?;
        let mut leases = Vec::new();
        let mut rows_seen = 0_usize;
        loop {
            if expired() {
                return Ok(None);
            }
            let Some(row) = rows.next()? else {
                break;
            };
            if rows_seen >= entry_budget {
                return Ok(None);
            }
            rows_seen = rows_seen.saturating_add(1);
            let id = row.get::<_, String>(0)?;
            if let Some(mut lease) = load_lease(&self.conn, "lease_id", &id)? {
                if lease.state == HostLeaseState::Active && lease.expires_at <= now_ms() {
                    lease.state = HostLeaseState::Quarantined;
                }
                leases.push(lease);
            }
        }
        if expired() {
            Ok(None)
        } else {
            Ok(Some(leases))
        }
    }
}

struct ResourceChild {
    child: std::process::Child,
    #[cfg(unix)]
    signals: BlockedSignals,
}

/// The command leader's exit status and the best available observation of its
/// original process group after the leader has been reaped.
///
/// On Unix this observes only the group created for the direct child. A
/// descendant that deliberately leaves the group, for example with `setsid`
/// or `setpgid`, is outside this bounded quiescence check. Other platforms
/// cannot observe that group and therefore report `Unknown` rather than claim
/// it is gone.
struct SupervisedProcessCompletion {
    status: std::process::ExitStatus,
    process_group: ProcessGroupObservation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProcessGroupObservation {
    Absent,
    Present,
    Unknown,
}

impl ProcessGroupObservation {
    fn description(self) -> &'static str {
        match self {
            Self::Absent => "was observed absent",
            Self::Present => "is still present",
            Self::Unknown => "could not be confirmed absent",
        }
    }
}

fn process_completion(
    process: &ResourceChild,
    status: std::process::ExitStatus,
) -> SupervisedProcessCompletion {
    SupervisedProcessCompletion {
        status,
        process_group: observe_owned_process_group(process.child.id()),
    }
}

#[cfg(unix)]
fn observe_owned_process_group(process_group_id: u32) -> ProcessGroupObservation {
    // The group leader has already been reaped. Signal 0 only probes whether
    // this numeric process group exists; never send a delivered signal after
    // reaping, when the identifier could eventually be reused.
    // SAFETY: `killpg` takes the group id and signal by value and dereferences
    // no memory owned by this crate. Signal 0 queries existence without
    // delivering a signal.
    let result = unsafe { libc::killpg(process_group_id as libc::pid_t, 0) };
    if result == 0 {
        ProcessGroupObservation::Present
    } else if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        ProcessGroupObservation::Absent
    } else {
        ProcessGroupObservation::Unknown
    }
}

#[cfg(not(unix))]
fn observe_owned_process_group(_process_group_id: u32) -> ProcessGroupObservation {
    // The broker CLI is not supported on Windows, and this platform has no
    // process-group observation here. Do not convert lack of proof into
    // quiescence if the library is compiled or used independently.
    ProcessGroupObservation::Unknown
}

const AUTHORITY_LOSS_TERM_GRACE: std::time::Duration = std::time::Duration::from_secs(1);
const AUTHORITY_LOSS_KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(2);
const CHILD_REAP_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

fn wait_for_child_exit(
    process: &mut ResourceChild,
    timeout: std::time::Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = process.child.try_wait()? {
            return Ok(Some(status));
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        std::thread::sleep((deadline - now).min(CHILD_REAP_POLL_INTERVAL));
    }
}

#[cfg(unix)]
fn signal_unreaped_process_group(
    process: &ResourceChild,
    signal: libc::c_int,
) -> std::io::Result<()> {
    // This helper is called only after a bounded `try_wait` loop observed the
    // leader unreaped. Its pid still names the original group and cannot have
    // been reused; no delivered signal is sent after `try_wait` reports exit.
    // SAFETY: the group was created by `process_group(0)` before spawn, and
    // `killpg` takes both values without dereferencing crate-owned memory.
    if unsafe { libc::killpg(process.child.id() as libc::pid_t, signal) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn kill_unreaped_child(process: &mut ResourceChild) -> std::io::Result<()> {
    // This library can be compiled on platforms where this broker CLI is not
    // supported. `Child::kill` is bounded and targets only the owned child.
    process.child.kill()
}

fn terminate_after_authority_loss<F>(
    process: &mut ResourceChild,
    event: &mut F,
) -> std::io::Result<Option<std::process::ExitStatus>>
where
    F: FnMut(&str),
{
    #[cfg(unix)]
    {
        event("sending SIGTERM to the still-owned supervised process group");
        if let Err(error) = signal_unreaped_process_group(process, libc::SIGTERM) {
            event(&format!("SIGTERM request failed: {error}"));
        }
    }
    #[cfg(not(unix))]
    {
        event("force-killing the still-owned supervised child on this platform");
        if let Err(error) = kill_unreaped_child(process) {
            event(&format!("direct-child kill request failed: {error}"));
        }
    }

    if let Some(status) = wait_for_child_exit(process, AUTHORITY_LOSS_TERM_GRACE)? {
        return Ok(Some(status));
    }

    #[cfg(unix)]
    let signal_error = {
        // `wait_for_child_exit` ended with `try_wait() == None`, so the leader
        // remains unreaped. Only now escalate, and never signal this group once
        // a status has been collected.
        event("SIGTERM grace expired; sending SIGKILL to the unreaped process group");
        let result = signal_unreaped_process_group(process, libc::SIGKILL);
        if let Err(error) = &result {
            event(&format!("SIGKILL request failed: {error}"));
        }
        result.err()
    };
    #[cfg(not(unix))]
    let signal_error = {
        event("direct-child kill grace expired; retrying kill while it remains unreaped");
        let result = kill_unreaped_child(process);
        if let Err(error) = &result {
            event(&format!("direct-child kill retry failed: {error}"));
        }
        result.err()
    };

    let status = wait_for_child_exit(process, AUTHORITY_LOSS_KILL_GRACE)?;
    match (status, signal_error) {
        (None, Some(error)) => Err(error),
        (status, _) => Ok(status),
    }
}

fn quarantine_run_lease_observed<O>(
    coordinator: &mut HostResourceCoordinator,
    grant: &HostResourceGrant,
    observe_phase: &mut O,
) -> Result<HostResourceLease, HostResourceError>
where
    O: FnMut(&'static str, u128),
{
    let started = std::time::Instant::now();
    let result = coordinator.quarantine(
        &grant.lease.lease_id,
        grant.lease.generation,
        &grant.ownership_token,
    );
    observe_phase("lease_finalization", started.elapsed().as_millis());
    result
}

fn quarantine_unconfirmed_termination<F, O>(
    coordinator: &mut HostResourceCoordinator,
    grant: &mut HostResourceGrant,
    detail: String,
    event: &mut F,
    observe_phase: &mut O,
) -> HostResourceRunError
where
    F: FnMut(&str),
    O: FnMut(&'static str, u128),
{
    let lease_id = grant.lease.lease_id.clone();
    grant.lease.state = HostLeaseState::Quarantined;
    let lease_state = match quarantine_run_lease_observed(coordinator, grant, observe_phase) {
        Ok(lease) => {
            grant.lease = lease;
            grant.lease.state.as_str().to_string()
        }
        Err(error) => format!("quarantine could not be confirmed: {error}"),
    };
    event(&format!(
        "supervised child status unavailable; lease {lease_id} {lease_state}"
    ));
    HostResourceRunError::TerminationUnconfirmed {
        lease_id,
        lease_state,
        detail,
    }
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

fn supervise_process<F, O>(
    coordinator: &mut HostResourceCoordinator,
    grant: &mut HostResourceGrant,
    ttl_seconds: u64,
    process: &mut ResourceChild,
    event: &mut F,
    observe_phase: &mut O,
) -> Result<SupervisedProcessCompletion, HostResourceRunError>
where
    F: FnMut(&str),
    O: FnMut(&'static str, u128),
{
    let renewal_interval = std::time::Duration::from_secs((ttl_seconds / 3).max(1));
    let mut next_renewal = std::time::Instant::now() + renewal_interval;
    loop {
        let status = match process.child.try_wait() {
            Ok(status) => status,
            Err(source) => {
                return Err(quarantine_unconfirmed_termination(
                    coordinator,
                    grant,
                    format!(
                        "failed to observe child exit; the child may still be running: {source}"
                    ),
                    event,
                    observe_phase,
                ));
            }
        };
        if let Some(status) = status {
            return Ok(process_completion(process, status));
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
                    let status = match terminate_after_authority_loss(process, event) {
                        Ok(Some(status)) => status,
                        Ok(None) => {
                            return Err(quarantine_unconfirmed_termination(
                                coordinator,
                                grant,
                                format!(
                                    "leader did not exit within the bounded {}ms TERM and {}ms KILL waits",
                                    AUTHORITY_LOSS_TERM_GRACE.as_millis(),
                                    AUTHORITY_LOSS_KILL_GRACE.as_millis()
                                ),
                                event,
                                observe_phase,
                            ));
                        }
                        Err(source) => {
                            return Err(quarantine_unconfirmed_termination(
                                coordinator,
                                grant,
                                format!("failed to observe child exit: {source}"),
                                event,
                                observe_phase,
                            ));
                        }
                    };
                    return Ok(process_completion(process, status));
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

fn validate_managed_storage_schema_supported(conn: &Connection) -> Result<(), HostResourceError> {
    preflight_managed_storage_schema_through(conn, MANAGED_STORAGE_SCHEMA_VERSION)?;
    let version: i64 = conn.query_row(
        "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
        [],
        |row| row.get(0),
    )?;
    let mut tables = vec![
        "managed_storage_policies",
        "managed_storage_claims",
        "managed_storage_claim_lineage",
    ];
    if version >= 2 {
        tables.extend([
            "managed_storage_claim_groups",
            "managed_storage_claim_group_members",
        ]);
    }
    if version >= 3 {
        tables.extend([
            "managed_storage_preparation_producers",
            "managed_storage_preparation_steps",
        ]);
    }
    if version >= 4 {
        tables.extend([
            "managed_storage_preparation_settlement_proofs",
            "managed_storage_adoptions",
        ]);
    }
    for table in tables {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(HostResourceError::InvalidRequest(format!(
                "managed-storage schema {version} is missing {table}"
            )));
        }
    }
    if version == i64::from(MANAGED_STORAGE_SCHEMA_VERSION) {
        validate_managed_storage_schema(conn)?;
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
    for table in [
        "managed_storage_claim_groups",
        "managed_storage_claim_group_members",
        "managed_storage_preparation_producers",
        "managed_storage_preparation_steps",
        "managed_storage_preparation_settlement_proofs",
        "managed_storage_adoptions",
    ] {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(HostResourceError::InvalidRequest(format!(
                "managed-storage schema {version} is missing {table}"
            )));
        }
    }
    for trigger in [
        "managed_storage_preparation_producer_identity_immutable",
        "managed_storage_preparation_producer_state_order",
        "managed_storage_preparation_step_identity_immutable",
        "managed_storage_preparation_step_state_order",
        "managed_storage_preparation_producer_retain",
        "managed_storage_preparation_step_retain",
        "managed_storage_preparation_settlement_proof_insert",
        "managed_storage_preparation_settlement_proof_update",
        "managed_storage_preparation_settlement_proof_delete",
        "managed_storage_adoption_update",
        "managed_storage_adoption_delete",
        "managed_storage_preparation_claim_insert_fence",
        "managed_storage_preparation_claim_update_fence",
        "managed_storage_preparation_claim_delete_fence",
        "managed_storage_preparation_lineage_insert_fence",
        "managed_storage_preparation_lineage_update_fence",
        "managed_storage_preparation_lineage_delete_fence",
        "managed_storage_preparation_group_update_fence",
        "managed_storage_preparation_group_delete_fence",
        "managed_storage_preparation_group_member_insert_fence",
        "managed_storage_preparation_group_member_update_fence",
        "managed_storage_preparation_group_member_delete_fence",
        "managed_storage_preparation_lease_identity_fence",
        "managed_storage_preparation_lease_state_fence",
        "managed_storage_preparation_lease_delete_fence",
        "managed_storage_preparation_allocation_insert_fence",
        "managed_storage_preparation_allocation_update_fence",
        "managed_storage_preparation_allocation_delete_fence",
        "managed_storage_preparation_reaper_skip",
    ] {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND name=?1)",
            [trigger],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(HostResourceError::InvalidRequest(format!(
                "managed-storage schema {version} is missing trigger {trigger}"
            )));
        }
    }
    Ok(())
}

/// Refuse unsupported managed schemas before applying this binary's table/index definitions.
/// The writable migration repeats this under its transaction to close the
/// race with another opener between preflight and acquiring the write lock.
fn preflight_managed_storage_schema_through(
    conn: &Connection,
    max_supported_version: u32,
) -> Result<(), HostResourceError> {
    let version: Option<rusqlite::types::Value> = conn
        .query_row(
            "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let version = match version {
        Some(rusqlite::types::Value::Integer(version)) => version,
        Some(_) => {
            return Err(HostResourceError::InvalidRequest(
                "managed storage schema version is not an integer".into(),
            ));
        }
        None => {
            return Err(HostResourceError::InvalidRequest(
                "managed storage schema version is missing".into(),
            ));
        }
    };
    if !(1..=i64::from(max_supported_version)).contains(&version) {
        return Err(HostResourceError::InvalidRequest(format!(
            "unsupported managed storage schema {version}"
        )));
    }
    Ok(())
}

fn preflight_existing_store_versions(conn: &Connection) -> Result<(), HostResourceError> {
    preflight_existing_store_versions_through(conn, MANAGED_STORAGE_SCHEMA_VERSION)
}

fn preflight_existing_store_versions_through(
    conn: &Connection,
    max_managed_schema_version: u32,
) -> Result<(), HostResourceError> {
    for (table, managed) in [("meta", false), ("managed_storage_meta", true)] {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )?;
        if exists {
            if managed {
                preflight_managed_storage_schema_through(conn, max_managed_schema_version)?;
            } else {
                validate_schema(conn)?;
            }
        }
    }
    Ok(())
}

fn migrate_managed_storage_schema(tx: &rusqlite::Transaction<'_>) -> Result<(), HostResourceError> {
    let mut version: i64 = tx.query_row(
        "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
        [],
        |row| row.get(0),
    )?;
    if version == i64::from(MANAGED_STORAGE_SCHEMA_VERSION) {
        return validate_managed_storage_schema(tx);
    }
    if version == 1 {
        tx.execute_batch(MANAGED_STORAGE_GROUP_SCHEMA)?;
        let updated = tx.execute(
            "UPDATE managed_storage_meta SET value=2
             WHERE key='schema_version' AND value=1",
            [],
        )?;
        if updated != 1 {
            return Err(HostResourceError::InvalidRequest(
                "managed-storage schema changed during its v1-to-v2 migration".into(),
            ));
        }
        version = 2;
    }
    if version == 2 {
        tx.execute_batch(MANAGED_STORAGE_PRODUCER_SCHEMA)?;
        let updated = tx.execute(
            "UPDATE managed_storage_meta SET value=?1
             WHERE key='schema_version' AND value=2",
            [3_i64],
        )?;
        if updated != 1 {
            return Err(HostResourceError::InvalidRequest(
                "managed-storage schema changed during its v2-to-v3 migration".into(),
            ));
        }
        version = 3;
    }
    if version == 3 {
        tx.execute_batch(MANAGED_STORAGE_PREPARATION_SETTLEMENT_PROOF_SCHEMA)?;
        tx.execute_batch(MANAGED_STORAGE_PREPARATION_SETTLEMENT_TRIGGER_DROP)?;
        tx.execute_batch(&managed_storage_producer_fence_schema())?;
        tx.execute_batch(MANAGED_STORAGE_ADOPTION_SCHEMA)?;
        let updated = tx.execute(
            "UPDATE managed_storage_meta SET value=?1
             WHERE key='schema_version' AND value=3",
            [i64::from(MANAGED_STORAGE_SCHEMA_VERSION)],
        )?;
        if updated != 1 {
            return Err(HostResourceError::InvalidRequest(
                "managed-storage schema changed during its v3-to-v4 migration".into(),
            ));
        }
        version = i64::from(MANAGED_STORAGE_SCHEMA_VERSION);
    }
    if version != i64::from(MANAGED_STORAGE_SCHEMA_VERSION) {
        return Err(HostResourceError::InvalidRequest(format!(
            "unsupported managed storage schema {version}"
        )));
    }
    validate_managed_storage_schema(tx)
}

fn migrate_writable_schema(conn: &mut Connection) -> Result<(), HostResourceError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    preflight_existing_store_versions(&tx)?;
    tx.execute_batch(SCHEMA)?;
    let version = store_schema_version(&tx)?;
    match version {
        1 => {
            tx.execute_batch(SUPERVISED_HOLD_MIGRATION)?;
            tx.execute(
                "UPDATE meta SET value=?1 WHERE key='schema_version'",
                [i64::from(HOST_RESOURCE_STORE_SCHEMA_VERSION)],
            )?;
        }
        version if version == HOST_RESOURCE_STORE_SCHEMA_VERSION => {}
        version => {
            return Err(HostResourceError::InvalidRequest(format!(
                "unsupported host schema {version}"
            )));
        }
    }
    validate_schema(&tx)?;
    validate_managed_storage_schema_supported(&tx)?;
    tx.commit()?;
    Ok(())
}

fn store_schema_version(conn: &Connection) -> Result<u32, HostResourceError> {
    let version: i64 = conn.query_row(
        "SELECT value FROM meta WHERE key='schema_version'",
        [],
        |row| row.get(0),
    )?;
    u32::try_from(version).map_err(|_| {
        HostResourceError::InvalidRequest(format!("unsupported host schema {version}"))
    })
}

fn has_supervised_capacity_hold(
    conn: &Connection,
    lease_id: &str,
    generation: u64,
) -> Result<bool, HostResourceError> {
    if store_schema_version(conn)? < HOST_RESOURCE_STORE_SCHEMA_VERSION {
        return Ok(false);
    }
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM resource_supervised_capacity_holds \
         WHERE lease_id=?1 AND generation=?2)",
        params![lease_id, generation as i64],
        |row| row.get(0),
    )?)
}

fn validate_schema(conn: &Connection) -> Result<u32, HostResourceError> {
    let version = store_schema_version(conn)?;
    match version {
        1 => Ok(version),
        HOST_RESOURCE_STORE_SCHEMA_VERSION => {
            for object in [
                "resource_supervised_capacity_holds",
                "resource_supervised_hold_capacity_insert",
                "resource_supervised_hold_capacity_delete",
                "resource_supervised_hold_capacity_update",
                "resource_supervised_hold_lease_release",
                "resource_supervised_hold_lease_delete",
            ] {
                let exists: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
                    [object],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Err(HostResourceError::InvalidRequest(format!(
                        "host schema {version} is missing {object}"
                    )));
                }
            }
            Ok(version)
        }
        _ => Err(HostResourceError::InvalidRequest(format!(
            "unsupported host schema {version}"
        ))),
    }
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

fn supervised_request_digest(request: &HostResourceRequest) -> Result<String, HostResourceError> {
    let generic_digest = request_digest(request)?;
    let mut digest = Sha256::new();
    digest.update(b"aethyme-host-resource-supervised/v1\0");
    digest.update(generic_digest.as_bytes());
    Ok(format!("{:x}", digest.finalize()))
}

struct Occupied {
    lease_id: String,
    generation: i64,
    kind: String,
    value: String,
    units: Option<u32>,
    limit: Option<u32>,
    holder_pid: Option<i64>,
    /// A named resource stays reserved after its holder dies, so the waiter needs
    /// to be told that waiting cannot resolve it.
    holder_gone: bool,
    /// A supervised run keeps capacity reserved until observed group absence or
    /// generation-bound reconciliation, even after its coordinator PID dies.
    supervised_hold: bool,
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
    let schema_version = store_schema_version(conn)?;
    let mut stmt = if schema_version >= HOST_RESOURCE_STORE_SCHEMA_VERSION {
        conn.prepare(
            "SELECT a.lease_id,l.generation,a.kind,a.value,a.units,a.capacity_limit,l.holder_pid,\
             EXISTS(SELECT 1 FROM resource_supervised_capacity_holds h \
                    WHERE h.lease_id=l.lease_id AND h.generation=l.generation) \
             FROM resource_allocations a JOIN resource_leases l ON l.lease_id=a.lease_id \
             WHERE l.state IN ('active','quarantined') ORDER BY l.generation,a.resource_key",
        )?
    } else {
        conn.prepare(
            "SELECT a.lease_id,l.generation,a.kind,a.value,a.units,a.capacity_limit,l.holder_pid,0 \
             FROM resource_allocations a JOIN resource_leases l ON l.lease_id=a.lease_id \
             WHERE l.state IN ('active','quarantined') ORDER BY l.generation,a.resource_key",
        )?
    };
    let rows = stmt
        .query_map([], |row| {
            let holder_pid = row.get::<_, Option<i64>>(6)?;
            Ok(Occupied {
                lease_id: row.get(0)?,
                generation: row.get(1)?,
                kind: row.get(2)?,
                value: row.get(3)?,
                units: row.get::<_, Option<i64>>(4)?.map(|v| v as u32),
                limit: row.get::<_, Option<i64>>(5)?.map(|v| v as u32),
                holder_pid,
                holder_gone: holder_pid.is_some_and(holder_process_is_gone),
                supervised_hold: row.get(7)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|occupied| {
            occupied.kind != "capacity"
                || occupied.supervised_hold
                || !occupied.holder_pid.is_some_and(holder_process_is_gone)
        })
        .collect())
}

fn plan_allocations(
    request: &HostResourceRequest,
    occupied: &[Occupied],
) -> Result<(Vec<HostResourceAllocation>, Vec<HostResourceConflict>), HostResourceError> {
    #[derive(Clone, Copy)]
    struct CapacityBundle {
        limit: u32,
        requested_units: u128,
        conflicting_limits: bool,
    }

    let mut capacity_bundles = BTreeMap::<String, CapacityBundle>::new();
    for requirement in &request.resources {
        if let HostResourceKind::Capacity { pool, units, limit } = &requirement.resource {
            let bundle = capacity_bundles
                .entry(pool.clone())
                .or_insert(CapacityBundle {
                    limit: *limit,
                    requested_units: 0,
                    conflicting_limits: false,
                });
            bundle.conflicting_limits |= bundle.limit != *limit;
            bundle.requested_units = bundle.requested_units.saturating_add(u128::from(*units));
        }
    }

    let mut proposed = Vec::new();
    let mut conflicts = Vec::new();
    let mut checked_capacity_pools = BTreeSet::new();
    let mut rejected_capacity_pools = BTreeSet::new();
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
                // Nothing holds a port between iterations, so a port this
                // bundle already chose still binds: exclude it explicitly, or
                // every request over an overlapping range gets the same one
                // (#1235).
                let port = (*start..=*end).find(|p| {
                    let value = p.to_string();
                    !occupied
                        .iter()
                        .any(|o| o.kind == "tcp_port" && o.value == value)
                        && !proposed.iter().any(|a: &HostResourceAllocation| {
                            a.kind == "tcp_port" && a.value == value
                        })
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
                if checked_capacity_pools.insert(pool.clone()) {
                    let bundle = capacity_bundles.get(pool).ok_or_else(|| {
                        HostResourceError::InvalidRequest("capacity pool summary is missing".into())
                    })?;
                    let matching = occupied
                        .iter()
                        .filter(|o| o.kind == "capacity" && o.value == *pool)
                        .collect::<Vec<_>>();
                    let mut rejected = false;
                    if bundle.conflicting_limits {
                        conflicts.push(conflict(
                            "capacity_policy_mismatch",
                            &requirement.key,
                            "capacity",
                            format!(
                                "{pool}:{} units with conflicting limits",
                                bundle.requested_units
                            ),
                            "the request bundle declares different limits for this capacity pool",
                            None,
                        ));
                        rejected = true;
                    }
                    if let Some(owner) = matching.iter().find(|o| o.limit != Some(bundle.limit)) {
                        conflicts.push(conflict(
                            "capacity_policy_mismatch",
                            &requirement.key,
                            "capacity",
                            format!("{pool}:{}/{limit}", bundle.requested_units),
                            "active pool uses a different limit",
                            Some(owner.lease_id.clone()),
                        ));
                        rejected = true;
                    }
                    if !rejected {
                        let used = matching
                            .iter()
                            .filter_map(|o| o.units)
                            .fold(0u128, |total, units| {
                                total.saturating_add(u128::from(units))
                            });
                        if used.saturating_add(bundle.requested_units) > u128::from(bundle.limit) {
                            conflicts.push(conflict(
                                "resource_contention",
                                &requirement.key,
                                "capacity",
                                format!("{pool}:{}/{limit}", bundle.requested_units),
                                &format!(
                                    "pool has {used}/{limit} units allocated by {} live lease(s); this bundle requests {} units",
                                    matching.len(), bundle.requested_units
                                ),
                                matching.first().map(|o| o.lease_id.clone()),
                            ));
                            rejected = true;
                        }
                    }
                    if rejected {
                        rejected_capacity_pools.insert(pool.clone());
                    }
                }
                if rejected_capacity_pools.contains(pool) {
                    continue;
                }
                proposed.push(allocation(
                    &requirement.key,
                    "capacity",
                    pool.clone(),
                    Some(*units),
                    Some(*limit),
                ));
            }
            HostResourceKind::ExclusiveKey { name } => {
                if let Some(owner) = occupied
                    .iter()
                    .find(|o| o.kind == "exclusive_key" && o.value == *name)
                {
                    let reason = format!("exclusive key is held by lease {}", owner.lease_id);
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
    mark_orphaned_holder_conflicts(&mut conflicts, occupied);
    Ok((proposed, conflicts))
}

fn quarantine_conflicting_orphaned_holders(
    tx: &rusqlite::Transaction<'_>,
    now: i64,
    conflicts: &[HostResourceConflict],
    occupied: &[Occupied],
) -> Result<(), HostResourceError> {
    for conflict in conflicts
        .iter()
        .filter(|conflict| conflict.code == ORPHANED_HOLDER_CONFLICT_CODE)
    {
        let Some(lease_id) = conflict.owning_lease.as_deref() else {
            continue;
        };
        let Some(holder) = occupied.iter().find(|owner| {
            owner.lease_id == lease_id
                && owner.kind == "exclusive_key"
                && owner.holder_gone
                && owner.holder_pid.is_some()
        }) else {
            continue;
        };
        let Some(holder_pid) = holder.holder_pid else {
            continue;
        };
        tx.execute(
            "UPDATE resource_leases SET state='quarantined',updated_at=?4 \
             WHERE lease_id=?1 AND generation=?2 AND holder_pid=?3 AND state='active'",
            params![lease_id, holder.generation, holder_pid, now],
        )?;
    }
    Ok(())
}

fn mark_orphaned_holder_conflicts(conflicts: &mut [HostResourceConflict], occupied: &[Occupied]) {
    for conflict in conflicts {
        if conflict.kind != "exclusive_key" {
            continue;
        }
        let Some(holder) = occupied.iter().find(|owner| {
            owner.kind == "exclusive_key" && owner.value == conflict.requested && owner.holder_gone
        }) else {
            continue;
        };

        conflict.code = ORPHANED_HOLDER_CONFLICT_CODE.into();
        conflict.owning_lease = Some(holder.lease_id.clone());
        let pid = holder.holder_pid.map_or_else(
            || "holder PID unknown".into(),
            |pid| format!("holder PID {pid}"),
        );
        conflict.reason = format!(
            "exclusive key is held by lease {} ({pid}) whose holder process is gone; \
             waiting cannot release it, review cleanup and run aethyme broker \
             advanced resources reconcile {} --confirm {}",
            holder.lease_id, holder.lease_id, holder.generation
        );
    }
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
        return "use one limit for this pool throughout the request and match any existing holder; waiting cannot change policy".into();
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
            action: "use one limit for this pool throughout the request and match any existing holder; waiting cannot change policy".into(),
        };
    }
    // Only an exact exclusive-key collision is made impossible by this
    // holder alone. A dead capacity holder or one port in a range can leave
    // room for the request when another live holder releases its allocation.
    if let Some(holder) = blockers
        .iter()
        .filter(|blocker| blocker.conflict.kind == "exclusive_key")
        .flat_map(|blocker| &blocker.holders)
        .find(|holder| holder.process_alive == Some(false))
    {
        return HostResourceWaitAdvice {
            waitable: false,
            reason: ORPHANED_HOLDER_CONFLICT_CODE.into(),
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
    } else if conflicts
        .iter()
        .any(|conflict| conflict.code == ORPHANED_HOLDER_CONFLICT_CODE)
    {
        ORPHANED_HOLDER_CONFLICT_CODE
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
    quarantine_reclaimable_with_diagnostics(conn, now).map(|report| report.report)
}

fn quarantine_reclaimable_with_diagnostics(
    conn: &Connection,
    now: i64,
) -> Result<HostResourceReapReportWithDiagnostics, HostResourceError> {
    quarantine_expired(conn, now)?;
    quarantine_dead_holders(conn, now)?;
    reclaim_dead_capacity_with_diagnostics(conn, now)
}

struct PendingReapCandidate {
    lease_id: String,
    generation: u64,
    holder_pid: i64,
    expires_at: i64,
    quarantined: bool,
    unresolved_preparation: bool,
}

/// Project active-to-quarantined transitions without writing. Use the same
/// settlement predicate as the insertion fence authorizing actual reclamation;
/// leave the existing reaper's legacy-registry behavior unchanged.
fn pending_reap_candidates(
    conn: &Connection,
) -> Result<Vec<PendingReapCandidate>, HostResourceError> {
    let managed_version: i64 = conn.query_row(
        "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
        [],
        |row| row.get(0),
    )?;
    let unresolved = match managed_version {
        // Producer records cannot exist in these known schemas.
        1 | 2 => "0".to_owned(),
        // Schema 3 has producers but no immutable settlement proofs. Keep
        // every linked lease fenced, regardless of a legacy terminal label.
        3 => "EXISTS (SELECT 1 FROM managed_storage_preparation_producers p
              WHERE p.lease_id=resource_leases.lease_id
                AND p.generation=resource_leases.generation)"
            .to_owned(),
        4 => managed_storage_unresolved_producer_predicate_sql(
            "resource_leases.lease_id",
            "resource_leases.generation",
        ),
        _ => {
            return Err(HostResourceError::InvalidRequest(format!(
                "unsupported managed storage schema {managed_version}"
            )));
        }
    };
    let sql = format!(
        "SELECT lease_id,generation,holder_pid,expires_at,state='quarantined',({unresolved})
         FROM resource_leases
         WHERE state IN ('quarantined','active') AND holder_pid IS NOT NULL
           AND NOT EXISTS (
               SELECT 1 FROM resource_reap_observations r
               WHERE r.lease_id=resource_leases.lease_id
           )
           AND NOT EXISTS (
               SELECT 1 FROM resource_supervised_capacity_holds h
               WHERE h.lease_id=resource_leases.lease_id
                 AND h.generation=resource_leases.generation
           )
         ORDER BY generation"
    );
    let mut stmt = conn.prepare(&sql)?;
    Ok(stmt
        .query_map([], |row| {
            Ok(PendingReapCandidate {
                lease_id: row.get(0)?,
                generation: row.get::<_, i64>(1)? as u64,
                holder_pid: row.get(2)?,
                expires_at: row.get(3)?,
                quarantined: row.get(4)?,
                unresolved_preparation: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

/// Reclaim only the allocation that becomes meaningless with the holder's
/// process: a capacity unit. Namespaces, ports, and exclusive keys remain on
/// the quarantined lease because they can correspond to residue on the host.
fn reclaim_dead_capacity_with_diagnostics(
    conn: &Connection,
    now: i64,
) -> Result<HostResourceReapReportWithDiagnostics, HostResourceError> {
    let candidates = {
        let mut stmt = conn.prepare(
            "SELECT lease_id,generation,holder_pid FROM resource_leases \
             WHERE state='quarantined' AND holder_pid IS NOT NULL \
             AND NOT EXISTS (\
                 SELECT 1 FROM resource_reap_observations r \
                 WHERE r.lease_id=resource_leases.lease_id\
             ) \
             AND NOT EXISTS (\
                 SELECT 1 FROM resource_supervised_capacity_holds h \
                 WHERE h.lease_id=resource_leases.lease_id \
                   AND h.generation=resource_leases.generation\
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
    let mut not_reaped = Vec::new();
    for (lease_id, generation, holder_pid) in candidates {
        if !holder_process_is_gone(holder_pid) {
            not_reaped.push(HostResourceNotReapedLease {
                lease_id,
                reason: HostResourceNotReapedReason::NotProvenGone,
            });
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
    Ok(HostResourceReapReportWithDiagnostics { report, not_reaped })
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

    fn migration_snapshot_tables(version: i64) -> Vec<&'static str> {
        let mut tables = vec![
            "managed_storage_policies",
            "managed_storage_claims",
            "managed_storage_claim_lineage",
        ];
        if version >= 2 {
            tables.extend([
                "managed_storage_claim_groups",
                "managed_storage_claim_group_members",
            ]);
        }
        if version >= 3 {
            tables.extend([
                "managed_storage_preparation_producers",
                "managed_storage_preparation_steps",
            ]);
        }
        tables
    }

    fn table_rows(conn: &Connection, table: &str) -> Vec<Vec<rusqlite::types::Value>> {
        let mut statement = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let column_count = statement.column_count();
        statement
            .query_map([], |row| {
                (0..column_count)
                    .map(|column| row.get(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    fn migration_snapshot(
        conn: &Connection,
        version: i64,
    ) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
        migration_snapshot_tables(version)
            .into_iter()
            .map(|table| (table.to_owned(), table_rows(conn, table)))
            .collect()
    }

    fn seed_migration_legacy_rows(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO managed_storage_policies
                (repository,filesystem_id,managed_cap_bytes,free_reserve_bytes,policy_digest,updated_at)
             VALUES ('migration-repo','migration-fs',16384,512,'policy-v1',101);
             INSERT INTO managed_storage_claims
                (repository,filesystem_id,claim_id,request_digest,policy_digest,
                 policy_cap_bytes,policy_free_reserve_bytes,materialization,root_identity,
                 state,requested_bytes,reserved_bytes,measured_bytes,release_receipt_digest,
                 created_at,updated_at)
             VALUES ('migration-repo','migration-fs','legacy-claim','request-v1','policy-v1',
                     16384,512,'session-worktree','root-v1','charged',4096,0,3000,NULL,102,103);
             INSERT INTO managed_storage_claim_lineage
                (repository,filesystem_id,claim_id,predecessor_claim_id,baseline_bytes,
                 root_instance_id,superseded_by_claim_id,updated_at)
             VALUES ('migration-repo','migration-fs','legacy-claim',NULL,3000,
                     'legacy-root-instance',NULL,104);",
        )
        .unwrap();
    }

    fn seed_v3_managed_rows(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO managed_storage_claim_groups
                (repository,group_id,request_id,lease_id,generation,materialization,
                 root_set_digest,pre_inventory_digest,member_manifest_digest,manifest_digest,
                 member_count,state,receipt_digest,created_at,updated_at)
             VALUES ('migration-repo','legacy-group','legacy-request','legacy-lease',7,
                     'session-worktree',printf('%064d',1),printf('%064d',2),
                     printf('%064d',3),printf('%064d',4),1,'reserved',NULL,201,202);
             INSERT INTO managed_storage_claim_group_members
                (repository,group_id,ordinal,filesystem_id,claim_id,root_identity,
                 claim_request_digest,requested_bytes,reserved_bytes,predecessor_claim_id,
                 baseline_bytes,root_instance_id)
             VALUES ('migration-repo','legacy-group',0,'migration-fs','legacy-claim',
                     'root-v1',printf('%064d',5),4096,2048,NULL,3000,
                     'legacy-root-instance');
             INSERT INTO managed_storage_preparation_producers
                (repository,producer_id,group_id,request_id,lease_id,generation,
                 materialization,root_set_digest,pre_inventory_digest,member_manifest_digest,
                 manifest_digest,plan_digest,step_count,next_step_ordinal,state,created_at,updated_at)
             VALUES ('migration-repo',printf('%064d',6),'legacy-group','legacy-request',
                     'legacy-lease',7,'session-worktree',printf('%064d',1),
                     printf('%064d',2),printf('%064d',3),printf('%064d',4),
                     printf('%064d',7),1,0,'active',203,204);
             INSERT INTO managed_storage_preparation_steps
                (repository,producer_id,ordinal,name,step_digest,state,intent_token,
                 outcome_code,exit_code,terminal_digest,created_at,updated_at)
             VALUES ('migration-repo',printf('%064d',6),0,'legacy-step',
                     printf('%064d',8),'planned',NULL,NULL,NULL,NULL,205,206);",
        )
        .unwrap();
    }

    fn seed_v3_root_mutation_rows(conn: &Connection, settled_without_proof: bool) {
        conn.execute_batch(
            "INSERT INTO managed_storage_claims
                (repository,filesystem_id,claim_id,request_digest,policy_digest,
                 policy_cap_bytes,policy_free_reserve_bytes,materialization,root_identity,
                 state,requested_bytes,reserved_bytes,measured_bytes,release_receipt_digest,
                 created_at,updated_at)
             VALUES ('migration-repo','migration-fs','legacy-next','request-next','policy-v1',
                     16384,512,'session-worktree','root-v1','reserved',1024,1024,NULL,NULL,210,211);
             INSERT INTO managed_storage_claim_groups
                (repository,group_id,request_id,lease_id,generation,materialization,
                 root_set_digest,pre_inventory_digest,member_manifest_digest,manifest_digest,
                 member_count,state,receipt_digest,created_at,updated_at)
             VALUES ('migration-repo','successor-group','successor-request','successor-lease',8,
                     'session-worktree',printf('%064d',11),printf('%064d',12),
                     printf('%064d',13),printf('%064d',14),1,'reserved',NULL,212,213);
             INSERT INTO resource_leases
                (lease_id,request_id,request_digest,repository,worktree_fingerprint,run_id,
                 generation,ownership_token,state,holder_pid,created_at,expires_at,updated_at,released_at)
             VALUES ('legacy-lease','legacy-request','legacy-digest','migration-repo',
                     'legacy-worktree','legacy-run',7,'legacy-token','active',123,200,400,201,NULL);",
        )
        .unwrap();
        if settled_without_proof {
            conn.execute_batch(
                "DROP TRIGGER managed_storage_preparation_group_update_fence;
                 UPDATE managed_storage_preparation_steps
                    SET state='terminal',intent_token=printf('%064d',15),
                        outcome_code='succeeded',exit_code=0,
                        terminal_digest=printf('%064d',16),updated_at=214
                  WHERE repository='migration-repo' AND producer_id=printf('%064d',6) AND ordinal=0;
                 UPDATE managed_storage_preparation_producers
                    SET state='steps_complete',next_step_ordinal=1,updated_at=215
                  WHERE repository='migration-repo' AND producer_id=printf('%064d',6);
                 UPDATE managed_storage_claim_groups
                    SET state='settled',receipt_digest=printf('%064d',17),updated_at=216
                  WHERE repository='migration-repo' AND group_id='legacy-group';",
            )
            .unwrap();
        }
    }

    fn seed_valid_settled_producer(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO managed_storage_claims
                (repository,filesystem_id,claim_id,request_digest,policy_digest,
                 policy_cap_bytes,policy_free_reserve_bytes,materialization,root_identity,
                 state,requested_bytes,reserved_bytes,measured_bytes,release_receipt_digest,
                 created_at,updated_at)
             VALUES ('proof-repo','proof-fs','proof-claim','request-proof','policy-proof',
                     16384,512,'session-worktree','root-proof','charged',4096,0,3000,NULL,301,302);
             INSERT INTO managed_storage_claim_lineage
                (repository,filesystem_id,claim_id,predecessor_claim_id,baseline_bytes,
                 root_instance_id,superseded_by_claim_id,updated_at)
             VALUES ('proof-repo','proof-fs','proof-claim',NULL,3000,
                     'root-instance-proof',NULL,303);
             INSERT INTO managed_storage_claim_groups
                (repository,group_id,request_id,lease_id,generation,materialization,
                 root_set_digest,pre_inventory_digest,member_manifest_digest,manifest_digest,
                 member_count,state,receipt_digest,created_at,updated_at)
             VALUES ('proof-repo','proof-group','proof-request','proof-lease',12,
                     'session-worktree',printf('%064d',21),printf('%064d',22),
                     printf('%064d',23),printf('%064d',24),1,'reserved',NULL,304,305);
             INSERT INTO managed_storage_claim_group_members
                (repository,group_id,ordinal,filesystem_id,claim_id,root_identity,
                 claim_request_digest,requested_bytes,reserved_bytes,predecessor_claim_id,
                 baseline_bytes,root_instance_id)
             VALUES ('proof-repo','proof-group',0,'proof-fs','proof-claim','root-proof',
                     printf('%064d',25),4096,2048,NULL,3000,'root-instance-proof');
             INSERT INTO managed_storage_preparation_producers
                (repository,producer_id,group_id,request_id,lease_id,generation,
                 materialization,root_set_digest,pre_inventory_digest,member_manifest_digest,
                 manifest_digest,plan_digest,step_count,next_step_ordinal,state,created_at,updated_at)
             VALUES ('proof-repo',printf('%064d',26),'proof-group','proof-request',
                     'proof-lease',12,'session-worktree',printf('%064d',21),
                     printf('%064d',22),printf('%064d',23),printf('%064d',24),
                     printf('%064d',27),1,0,'active',306,307);
             INSERT INTO managed_storage_preparation_steps
                (repository,producer_id,ordinal,name,step_digest,state,intent_token,
                 outcome_code,exit_code,terminal_digest,created_at,updated_at)
             VALUES ('proof-repo',printf('%064d',26),0,'proof-step',printf('%064d',28),
                     'planned',NULL,NULL,NULL,NULL,308,309);
             UPDATE managed_storage_preparation_producers
                SET state='intent',updated_at=310
              WHERE repository='proof-repo' AND producer_id=printf('%064d',26);
             UPDATE managed_storage_preparation_steps
                SET state='intent',intent_token=printf('%064d',29),updated_at=311
              WHERE repository='proof-repo' AND producer_id=printf('%064d',26) AND ordinal=0;
             UPDATE managed_storage_preparation_steps
                SET state='terminal',outcome_code='succeeded',exit_code=0,
                    terminal_digest=printf('%064d',30),updated_at=312
              WHERE repository='proof-repo' AND producer_id=printf('%064d',26) AND ordinal=0;
             UPDATE managed_storage_preparation_producers
                SET state='steps_complete',next_step_ordinal=1,updated_at=313
              WHERE repository='proof-repo' AND producer_id=printf('%064d',26);
             INSERT INTO managed_storage_preparation_settlement_proofs
                (repository,group_id,request_id,lease_id,generation,materialization,
                 root_set_digest,pre_inventory_digest,member_manifest_digest,manifest_digest,
                 producer_id,plan_digest,ordered_steps_digest,inventory_digest,
                 aggregate_digest,created_at)
             VALUES ('proof-repo','proof-group','proof-request','proof-lease',12,
                     'session-worktree',printf('%064d',21),printf('%064d',22),
                     printf('%064d',23),printf('%064d',24),printf('%064d',26),
                     printf('%064d',27),printf('%064d',31),printf('%064d',32),
                     printf('%064d',33),314);
             UPDATE managed_storage_claim_groups
                SET state='settled',receipt_digest=printf('%064d',33),updated_at=315
              WHERE repository='proof-repo' AND group_id='proof-group';",
        )
        .unwrap();
    }

    fn seed_old_v3_replaced_triggers(conn: &Connection) {
        for (name, event, table) in [
            (
                "managed_storage_preparation_claim_update_fence",
                "UPDATE",
                "managed_storage_claims",
            ),
            (
                "managed_storage_preparation_lineage_update_fence",
                "UPDATE",
                "managed_storage_claim_lineage",
            ),
            (
                "managed_storage_preparation_group_update_fence",
                "UPDATE",
                "managed_storage_claim_groups",
            ),
            (
                "managed_storage_preparation_lease_identity_fence",
                "UPDATE",
                "resource_leases",
            ),
            (
                "managed_storage_preparation_lease_state_fence",
                "UPDATE",
                "resource_leases",
            ),
            (
                "managed_storage_preparation_allocation_delete_fence",
                "DELETE",
                "resource_allocations",
            ),
            (
                "managed_storage_preparation_reaper_skip",
                "INSERT",
                "resource_reap_observations",
            ),
        ] {
            conn.execute_batch(&format!(
                "CREATE TRIGGER {name} BEFORE {event} ON {table}
                 BEGIN SELECT RAISE(ABORT, 'old schema-v3 fence'); END;"
            ))
            .unwrap();
        }
    }

    fn create_managed_schema_fixture(
        path: &std::path::Path,
        managed_version: i64,
    ) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
        assert!((1..=4).contains(&managed_version));
        let mut conn = Connection::open(path).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(SUPERVISED_HOLD_MIGRATION).unwrap();
        conn.execute("UPDATE meta SET value=2 WHERE key='schema_version'", [])
            .unwrap();
        seed_migration_legacy_rows(&conn);

        if managed_version >= 2 {
            conn.execute_batch(MANAGED_STORAGE_GROUP_SCHEMA).unwrap();
            conn.execute(
                "UPDATE managed_storage_meta SET value=2 WHERE key='schema_version'",
                [],
            )
            .unwrap();
        }
        if managed_version >= 3 {
            conn.execute_batch(MANAGED_STORAGE_PRODUCER_SCHEMA).unwrap();
            seed_v3_managed_rows(&conn);
            seed_old_v3_replaced_triggers(&conn);
            conn.execute(
                "UPDATE managed_storage_meta SET value=3 WHERE key='schema_version'",
                [],
            )
            .unwrap();
        }
        if managed_version == 4 {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            migrate_managed_storage_schema(&tx).unwrap();
            tx.commit().unwrap();
        }

        let snapshot = migration_snapshot(&conn, managed_version);
        drop(conn);
        snapshot
    }

    fn directory_entries(path: &std::path::Path) -> Vec<String> {
        let mut entries = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    fn managed_schema_objects(conn: &Connection) -> Vec<(String, String, Option<String>)> {
        let mut statement = conn
            .prepare(
                "SELECT type,name,sql FROM sqlite_master
                 WHERE name GLOB 'managed_storage_*' ORDER BY type,name",
            )
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    fn managed_schema_row_counts(conn: &Connection) -> Vec<(String, i64)> {
        [
            "managed_storage_policies",
            "managed_storage_claims",
            "managed_storage_claim_lineage",
            "managed_storage_claim_groups",
            "managed_storage_claim_group_members",
            "managed_storage_preparation_producers",
            "managed_storage_preparation_steps",
            "managed_storage_preparation_settlement_proofs",
            "managed_storage_adoptions",
        ]
        .into_iter()
        .map(|table| {
            let count = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            (table.to_owned(), count)
        })
        .collect()
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
        assert_eq!(host_version, i64::from(HOST_RESOURCE_STORE_SCHEMA_VERSION));
        assert_eq!(ledger_version, 1);
        preflight_managed_storage_schema_through(&coordinator.conn, 1).unwrap();
    }

    #[test]
    fn managed_storage_addition_preserves_existing_host_schema_v2() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host.db");
        let legacy = Connection::open(&db).unwrap();
        legacy
            .execute_batch(
                SCHEMA
                    .split("CREATE TABLE IF NOT EXISTS managed_storage_meta")
                    .next()
                    .unwrap(),
            )
            .unwrap();
        legacy.execute_batch(SUPERVISED_HOLD_MIGRATION).unwrap();
        legacy
            .execute("UPDATE meta SET value=2 WHERE key='schema_version'", [])
            .unwrap();
        drop(legacy);
        let coordinator = HostResourceCoordinator::open(&db).unwrap();
        assert_eq!(
            store_schema_version(&coordinator.conn).unwrap(),
            HOST_RESOURCE_STORE_SCHEMA_VERSION
        );
        validate_managed_storage_schema_supported(&coordinator.conn).unwrap();
        validate_schema(&coordinator.conn).unwrap();
        let managed_version: i64 = coordinator
            .conn
            .query_row(
                "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(managed_version, 1);
    }

    fn assert_managed_schema_migrates_from(version: i64) {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host.db");
        let before = create_managed_schema_fixture(&db, version);
        let before_connection = Connection::open(&db).unwrap();
        let before_objects = managed_schema_objects(&before_connection);
        let before_schema_cookie: i64 = before_connection
            .query_row("PRAGMA schema_version", [], |row| row.get(0))
            .unwrap();
        drop(before_connection);

        let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
        assert_eq!(managed_schema_objects(&coordinator.conn), before_objects);
        let opened_schema_cookie: i64 = coordinator
            .conn
            .query_row("PRAGMA schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(opened_schema_cookie, before_schema_cookie);
        assert_eq!(
            store_schema_version(&coordinator.conn).unwrap(),
            HOST_RESOURCE_STORE_SCHEMA_VERSION
        );
        assert_eq!(MANAGED_STORAGE_SCHEMA_VERSION, 4);
        let opened_version: i64 = coordinator
            .conn
            .query_row(
                "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            opened_version, version,
            "ordinary open must preserve the managed floor"
        );
        assert_eq!(migration_snapshot(&coordinator.conn, version), before);
        let confirmation = ManagedStorageSchemaActivationConfirmation::explicitly_confirmed(
            &db,
            ManagedStorageSchemaActivationConfirmation::ACKNOWLEDGEMENT,
        )
        .unwrap();
        coordinator
            .activate_managed_storage_schema(confirmation)
            .unwrap();
        validate_schema(&coordinator.conn).unwrap();
        validate_managed_storage_schema(&coordinator.conn).unwrap();
        assert_eq!(
            migration_snapshot(&coordinator.conn, version),
            before,
            "legacy rows must retain their SQLite values while explicitly migrating v{version} to v4"
        );
        if version == 3 {
            let old_guard_count: i64 = coordinator
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type='trigger' AND sql LIKE '%old schema-v3 fence%'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(old_guard_count, 0, "v3 fence triggers must be replaced");
        }
    }

    #[test]
    fn managed_storage_schema_v1_migrates_to_v4_preserving_legacy_rows() {
        assert_managed_schema_migrates_from(1);
    }

    #[test]
    fn managed_storage_schema_v2_migrates_to_v4_preserving_legacy_rows() {
        assert_managed_schema_migrates_from(2);
    }

    #[test]
    fn managed_storage_schema_v3_migrates_to_v4_preserving_legacy_rows() {
        assert_managed_schema_migrates_from(3);
    }

    #[test]
    fn managed_storage_schema_v4_open_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host.db");
        let expected_legacy = create_managed_schema_fixture(&db, 4);

        let first = HostResourceCoordinator::open(&db).unwrap();
        let first_objects = managed_schema_objects(&first.conn);
        let first_counts = managed_schema_row_counts(&first.conn);
        let first_schema_cookie: i64 = first
            .conn
            .query_row("PRAGMA schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            store_schema_version(&first.conn).unwrap(),
            HOST_RESOURCE_STORE_SCHEMA_VERSION
        );
        assert_eq!(migration_snapshot(&first.conn, 4), expected_legacy);
        drop(first);

        let second = HostResourceCoordinator::open(&db).unwrap();
        assert_eq!(
            store_schema_version(&second.conn).unwrap(),
            HOST_RESOURCE_STORE_SCHEMA_VERSION
        );
        validate_managed_storage_schema(&second.conn).unwrap();
        let second_schema_cookie: i64 = second
            .conn
            .query_row("PRAGMA schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(second_schema_cookie, first_schema_cookie);
        assert_eq!(managed_schema_objects(&second.conn), first_objects);
        assert_eq!(managed_schema_row_counts(&second.conn), first_counts);
        assert_eq!(migration_snapshot(&second.conn, 4), expected_legacy);
    }

    #[test]
    fn managed_storage_schema_activation_requires_exact_confirmation_and_same_database() {
        let temp = tempfile::tempdir().unwrap();
        let first_db = temp.path().join("first.db");
        let second_db = temp.path().join("second.db");
        create_managed_schema_fixture(&first_db, 1);
        create_managed_schema_fixture(&second_db, 1);
        let mut first = HostResourceCoordinator::open(&first_db).unwrap();
        let mut second = HostResourceCoordinator::open(&second_db).unwrap();

        assert!(
            ManagedStorageSchemaActivationConfirmation::explicitly_confirmed(
                &first_db,
                "not-an-activation-confirmation",
            )
            .is_err()
        );
        let first_confirmation = ManagedStorageSchemaActivationConfirmation::explicitly_confirmed(
            &first_db,
            ManagedStorageSchemaActivationConfirmation::ACKNOWLEDGEMENT,
        )
        .unwrap();
        assert!(
            second
                .activate_managed_storage_schema(first_confirmation)
                .is_err()
        );
        for coordinator in [&first, &second] {
            let version: i64 = coordinator
                .conn
                .query_row(
                    "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(version, 1);
        }

        first
            .activate_managed_storage_schema(
                ManagedStorageSchemaActivationConfirmation::explicitly_confirmed(
                    &first_db,
                    ManagedStorageSchemaActivationConfirmation::ACKNOWLEDGEMENT,
                )
                .unwrap(),
            )
            .unwrap();
        let activated_version: i64 = first
            .conn
            .query_row(
                "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(activated_version, 4);
    }

    #[cfg(unix)]
    #[test]
    fn managed_storage_schema_activation_refuses_database_alias_replacement() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let opened_db = temp.path().join("opened.db");
        let replacement_db = temp.path().join("replacement.db");
        let alias = temp.path().join("host.db");
        create_managed_schema_fixture(&opened_db, 1);
        create_managed_schema_fixture(&replacement_db, 1);
        symlink(&opened_db, &alias).unwrap();

        let mut coordinator = HostResourceCoordinator::open(&alias).unwrap();
        let confirmation = ManagedStorageSchemaActivationConfirmation::explicitly_confirmed(
            &alias,
            ManagedStorageSchemaActivationConfirmation::ACKNOWLEDGEMENT,
        )
        .unwrap();
        std::fs::remove_file(&alias).unwrap();
        symlink(&replacement_db, &alias).unwrap();

        assert!(
            coordinator
                .activate_managed_storage_schema(confirmation)
                .is_err()
        );
        for path in [&opened_db, &replacement_db] {
            let connection =
                Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            let version: i64 = connection
                .query_row(
                    "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                version, 1,
                "replacement refusal must not migrate either file"
            );
        }
    }

    #[test]
    fn managed_storage_schema_activation_anchors_relative_path_at_open() {
        let current_dir = std::env::current_dir().unwrap();
        let temp = tempfile::Builder::new()
            .prefix("managed-schema-activation-")
            .tempdir_in(&current_dir)
            .unwrap();
        let relative_db = temp
            .path()
            .strip_prefix(&current_dir)
            .unwrap()
            .join("host.db");
        let absolute_db = current_dir.join(&relative_db);
        create_managed_schema_fixture(&absolute_db, 1);

        let mut coordinator = HostResourceCoordinator::open(&relative_db).unwrap();
        assert_eq!(
            coordinator
                .database_identity
                .as_ref()
                .unwrap()
                .requested_path,
            absolute_db
        );
        let confirmation = ManagedStorageSchemaActivationConfirmation::explicitly_confirmed(
            &relative_db,
            ManagedStorageSchemaActivationConfirmation::ACKNOWLEDGEMENT,
        )
        .unwrap();
        coordinator
            .activate_managed_storage_schema(confirmation)
            .unwrap();
    }

    fn assert_v3_root_mutations_fenced(settled_without_proof: bool) {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("legacy-v3.db");
        create_managed_schema_fixture(&db, 3);
        let seed = Connection::open(&db).unwrap();
        seed_v3_root_mutation_rows(&seed, settled_without_proof);
        drop(seed);

        let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
        let opened_version: i64 = coordinator
            .conn
            .query_row(
                "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(opened_version, 3, "ordinary open must retain legacy v3");
        coordinator
            .activate_managed_storage_schema(
                ManagedStorageSchemaActivationConfirmation::explicitly_confirmed(
                    &db,
                    ManagedStorageSchemaActivationConfirmation::ACKNOWLEDGEMENT,
                )
                .unwrap(),
            )
            .unwrap();
        let proof_count: i64 = coordinator
            .conn
            .query_row(
                "SELECT COUNT(*) FROM managed_storage_preparation_settlement_proofs
                 WHERE repository='migration-repo' AND group_id='legacy-group'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(proof_count, 0, "migration must not fabricate a v4 proof");

        let insert_claim = coordinator.conn.execute(
            "INSERT INTO managed_storage_claims
                (repository,filesystem_id,claim_id,request_digest,policy_digest,
                 policy_cap_bytes,policy_free_reserve_bytes,materialization,root_identity,
                 state,requested_bytes,reserved_bytes,measured_bytes,release_receipt_digest,
                 created_at,updated_at)
             VALUES ('migration-repo','migration-fs','future-claim','request-future','policy-v1',
                     16384,512,'session-worktree','root-v1','reserved',512,512,NULL,NULL,301,302)",
            [],
        );
        assert!(
            insert_claim.is_err(),
            "unresolved producer must block claim insertion"
        );

        let update_claim = coordinator.conn.execute(
            "UPDATE managed_storage_claims SET state='cleanup_pending',updated_at=303
             WHERE repository='migration-repo' AND filesystem_id='migration-fs'
               AND claim_id='legacy-claim'",
            [],
        );
        assert!(
            update_claim.is_err(),
            "unresolved producer must block claim update"
        );

        let insert_lineage = coordinator.conn.execute(
            "INSERT INTO managed_storage_claim_lineage
                (repository,filesystem_id,claim_id,predecessor_claim_id,baseline_bytes,
                 root_instance_id,superseded_by_claim_id,updated_at)
             VALUES ('migration-repo','migration-fs','legacy-next','legacy-claim',3000,
                     'legacy-root-instance',NULL,304)",
            [],
        );
        assert!(
            insert_lineage.is_err(),
            "unresolved producer must block lineage insertion"
        );

        let update_lineage = coordinator.conn.execute(
            "UPDATE managed_storage_claim_lineage SET baseline_bytes=3001,updated_at=305
             WHERE repository='migration-repo' AND filesystem_id='migration-fs'
               AND claim_id='legacy-claim'",
            [],
        );
        assert!(
            update_lineage.is_err(),
            "unresolved producer must block lineage update"
        );

        let insert_member = coordinator.conn.execute(
            "INSERT INTO managed_storage_claim_group_members
                (repository,group_id,ordinal,filesystem_id,claim_id,root_identity,
                 claim_request_digest,requested_bytes,reserved_bytes,predecessor_claim_id,
                 baseline_bytes,root_instance_id)
             VALUES ('migration-repo','successor-group',0,'migration-fs','legacy-next','root-v1',
                     printf('%064d',18),1024,512,'legacy-claim',3000,'legacy-root-instance')",
            [],
        );
        assert!(
            insert_member.is_err(),
            "unresolved producer must block successor membership"
        );

        let release_lease = coordinator.conn.execute(
            "UPDATE resource_leases SET state='released',released_at=306,updated_at=306
             WHERE lease_id='legacy-lease' AND generation=7",
            [],
        );
        assert!(
            release_lease.is_err(),
            "unresolved producer must block lease release"
        );

        let claim_state: String = coordinator
            .conn
            .query_row(
                "SELECT state FROM managed_storage_claims
                 WHERE repository='migration-repo' AND filesystem_id='migration-fs'
                   AND claim_id='legacy-claim'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(claim_state, "charged");
        let lineage_baseline: i64 = coordinator
            .conn
            .query_row(
                "SELECT baseline_bytes FROM managed_storage_claim_lineage
                 WHERE repository='migration-repo' AND filesystem_id='migration-fs'
                   AND claim_id='legacy-claim'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lineage_baseline, 3000);
        let member_count: i64 = coordinator
            .conn
            .query_row(
                "SELECT COUNT(*) FROM managed_storage_claim_group_members
                 WHERE repository='migration-repo' AND group_id='successor-group'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(member_count, 0);
        let lease_state: String = coordinator
            .conn
            .query_row(
                "SELECT state FROM resource_leases WHERE lease_id='legacy-lease'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(lease_state, "active");
    }

    #[test]
    fn managed_storage_unresolved_producer_blocks_root_mutations_and_release() {
        assert_v3_root_mutations_fenced(false);
    }

    #[test]
    fn migrated_v3_settlement_without_proof_remains_root_fenced() {
        assert_v3_root_mutations_fenced(true);
    }

    #[test]
    fn valid_settlement_proof_allows_successor_root_claim_and_lineage() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("proof-settled.db");
        let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
        coordinator
            .activate_managed_storage_schema(
                ManagedStorageSchemaActivationConfirmation::explicitly_confirmed(
                    &db,
                    ManagedStorageSchemaActivationConfirmation::ACKNOWLEDGEMENT,
                )
                .unwrap(),
            )
            .unwrap();
        seed_valid_settled_producer(&coordinator.conn);

        coordinator
            .conn
            .execute(
                "UPDATE managed_storage_claims SET state='cleanup_pending',updated_at=316
                 WHERE repository='proof-repo' AND filesystem_id='proof-fs'
                   AND claim_id='proof-claim'",
                [],
            )
            .unwrap();
        coordinator
            .conn
            .execute(
                "UPDATE managed_storage_claim_lineage SET baseline_bytes=3001,updated_at=317
                 WHERE repository='proof-repo' AND filesystem_id='proof-fs'
                   AND claim_id='proof-claim'",
                [],
            )
            .unwrap();
        coordinator
            .conn
            .execute(
                "INSERT INTO managed_storage_claims
                    (repository,filesystem_id,claim_id,request_digest,policy_digest,
                     policy_cap_bytes,policy_free_reserve_bytes,materialization,root_identity,
                     state,requested_bytes,reserved_bytes,measured_bytes,release_receipt_digest,
                     created_at,updated_at)
                 VALUES ('proof-repo','proof-fs','successor-claim','request-successor','policy-proof',
                         16384,512,'session-worktree','root-proof','reserved',2048,1024,NULL,NULL,318,319)",
                [],
            )
            .unwrap();
        coordinator
            .conn
            .execute(
                "INSERT INTO managed_storage_claim_lineage
                    (repository,filesystem_id,claim_id,predecessor_claim_id,baseline_bytes,
                     root_instance_id,superseded_by_claim_id,updated_at)
                 VALUES ('proof-repo','proof-fs','successor-claim','proof-claim',3001,
                         'root-instance-proof',NULL,320)",
                [],
            )
            .unwrap();
        coordinator
            .conn
            .execute(
                "INSERT INTO managed_storage_claim_groups
                    (repository,group_id,request_id,lease_id,generation,materialization,
                     root_set_digest,pre_inventory_digest,member_manifest_digest,manifest_digest,
                     member_count,state,receipt_digest,created_at,updated_at)
                 VALUES ('proof-repo','successor-group','successor-request','successor-lease',13,
                         'session-worktree',printf('%064d',34),printf('%064d',35),
                         printf('%064d',36),printf('%064d',37),1,'reserved',NULL,321,322)",
                [],
            )
            .unwrap();
        coordinator
            .conn
            .execute(
                "INSERT INTO managed_storage_claim_group_members
                    (repository,group_id,ordinal,filesystem_id,claim_id,root_identity,
                     claim_request_digest,requested_bytes,reserved_bytes,predecessor_claim_id,
                     baseline_bytes,root_instance_id)
                 VALUES ('proof-repo','successor-group',0,'proof-fs','successor-claim','root-proof',
                         printf('%064d',38),2048,1024,'proof-claim',3001,'root-instance-proof')",
                [],
            )
            .unwrap();

        let group_member_count: i64 = coordinator
            .conn
            .query_row(
                "SELECT COUNT(*) FROM managed_storage_claim_group_members
                 WHERE repository='proof-repo' AND group_id='successor-group'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(group_member_count, 1);
    }

    #[test]
    fn managed_storage_schema_v1_preflight_refuses_v4_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host.db");
        create_managed_schema_fixture(&db, 4);
        let coordinator = HostResourceCoordinator::open(&db).unwrap();
        let bytes_before = std::fs::read(&db).unwrap();
        let entries_before = directory_entries(temp.path());

        // Model the exact v1 reader boundary: the old binary accepted only managed schema 1.
        let error = preflight_existing_store_versions_through(&coordinator.conn, 1).unwrap_err();
        assert!(matches!(
            error,
            HostResourceError::InvalidRequest(message)
                if message.contains("unsupported managed storage schema 4")
        ));
        assert_eq!(std::fs::read(&db).unwrap(), bytes_before);
        assert_eq!(directory_entries(temp.path()), entries_before);
    }

    #[test]
    fn managed_storage_schema_migration_rolls_back_partial_ddl() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host.db");
        let expected_legacy = create_managed_schema_fixture(&db, 1);
        let legacy = Connection::open(&db).unwrap();
        legacy
            .execute_batch(
                "CREATE INDEX managed_storage_groups_by_request
                 ON managed_storage_claims(repository,claim_id);",
            )
            .unwrap();
        drop(legacy);

        let mut coordinator = HostResourceCoordinator::open(&db).unwrap();
        assert!(
            coordinator
                .activate_managed_storage_schema(
                    ManagedStorageSchemaActivationConfirmation::explicitly_confirmed(
                        &db,
                        ManagedStorageSchemaActivationConfirmation::ACKNOWLEDGEMENT,
                    )
                    .unwrap(),
                )
                .is_err()
        );
        drop(coordinator);
        let after = Connection::open(&db).unwrap();
        let managed_version: i64 = after
            .query_row(
                "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(managed_version, 1);
        assert_eq!(migration_snapshot(&after, 1), expected_legacy);
        let created_group: i64 = after
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='table' AND name IN ('managed_storage_claim_groups',
                                                  'managed_storage_claim_group_members')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(created_group, 0, "partial migration DDL must roll back");
    }

    #[test]
    fn managed_storage_schema_missing_or_noninteger_version_refuses_without_writes() {
        for (name, insert, expected_message) in [
            ("missing", "", "managed storage schema version is missing"),
            (
                "noninteger",
                "INSERT INTO managed_storage_meta VALUES('schema_version','not-an-integer');",
                "managed storage schema version is not an integer",
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join(format!("{name}.db"));
            let malformed = Connection::open(&db).unwrap();
            malformed
                .execute_batch(&format!(
                    "CREATE TABLE managed_storage_meta (key TEXT PRIMARY KEY,value INTEGER NOT NULL);{insert}"
                ))
                .unwrap();
            drop(malformed);
            let bytes_before = std::fs::read(&db).unwrap();
            let entries_before = directory_entries(temp.path());

            let error = HostResourceCoordinator::open(&db).err().unwrap();
            assert!(matches!(
                error,
                HostResourceError::InvalidRequest(message) if message.contains(expected_message)
            ));
            assert_eq!(std::fs::read(&db).unwrap(), bytes_before);
            assert_eq!(directory_entries(temp.path()), entries_before);
        }
    }

    #[test]
    fn future_managed_storage_schema_is_refused_before_database_writes() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("future-managed.db");
        let future = Connection::open(&db).unwrap();
        future
            .execute_batch(
                "CREATE TABLE managed_storage_meta (key TEXT PRIMARY KEY,value INTEGER NOT NULL);
             INSERT INTO managed_storage_meta VALUES('schema_version',5);
             CREATE TABLE managed_storage_claims (future_column TEXT);",
            )
            .unwrap();
        drop(future);
        let before = std::fs::read(&db).unwrap();
        let entries_before = directory_entries(temp.path());
        let error = HostResourceCoordinator::open(&db).err().unwrap();
        assert!(
            matches!(error, HostResourceError::InvalidRequest(message) if message.contains("unsupported managed storage schema 5"))
        );
        assert_eq!(std::fs::read(&db).unwrap(), before);
        assert_eq!(directory_entries(temp.path()), entries_before);
        let readonly = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let created: i64 = readonly.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('meta','resource_leases','managed_storage_policies')", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(created, 0);
    }

    #[test]
    fn future_managed_schema_with_wal_refuses_without_changing_database_or_wal() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("future-managed-wal.db");
        let future = Connection::open(&db).unwrap();
        let journal_mode: String = future
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        future.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        future
            .execute_batch(
                "CREATE TABLE managed_storage_meta (key TEXT PRIMARY KEY,value INTEGER NOT NULL);
                 INSERT INTO managed_storage_meta VALUES('schema_version',5);
                 CREATE TABLE future_managed_data (value TEXT);
                 INSERT INTO future_managed_data VALUES('preserve this WAL-backed row');",
            )
            .unwrap();
        let wal = PathBuf::from(format!("{}-wal", db.display()));
        assert!(wal.is_file(), "fixture must retain an existing WAL");
        let db_before = std::fs::read(&db).unwrap();
        let wal_before = std::fs::read(&wal).unwrap();

        let error = match HostResourceCoordinator::open(&db) {
            Err(error) => error,
            Ok(_) => panic!("future managed schema unexpectedly opened"),
        };
        assert!(matches!(
            error,
            HostResourceError::InvalidRequest(message)
                if message.contains("unsupported managed storage schema 5")
        ));
        assert_eq!(std::fs::read(&db).unwrap(), db_before);
        assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
        let version: i64 = future
            .query_row(
                "SELECT value FROM managed_storage_meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 5);
    }

    #[test]
    fn future_host_schema_is_refused_before_managed_storage_is_created() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("future-host.db");
        let future = Connection::open(&db).unwrap();
        future.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY,value INTEGER NOT NULL); INSERT INTO meta VALUES('schema_version',3);").unwrap();
        drop(future);
        let before = std::fs::read(&db).unwrap();
        let error = HostResourceCoordinator::open(&db).err().unwrap();
        assert!(
            matches!(error, HostResourceError::InvalidRequest(message) if message.contains("unsupported host schema 3"))
        );
        assert_eq!(std::fs::read(&db).unwrap(), before);
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
        let remaining = c.list(false).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].lease_id, a.lease.lease_id);
        assert_eq!(remaining[0].state, HostLeaseState::Active);
        assert_eq!(remaining[0].allocations.len(), 1);
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

    #[test]
    fn same_pool_capacity_requirements_are_summed_for_bundle_admission() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let capacity = |key: &str, units, limit| HostResourceRequirement {
            key: key.into(),
            resource: HostResourceKind::Capacity {
                pool: "workers".into(),
                units,
                limit,
            },
        };

        let fitting = req(
            "fitting-bundle",
            vec![capacity("worker-a", 1, 3), capacity("worker-b", 2, 3)],
        );
        let plan = c.plan(&fitting).unwrap();
        assert!(plan.available);
        assert_eq!(plan.proposed.len(), 2);
        let grant = c.acquire(&fitting).unwrap();
        assert_eq!(grant.lease.allocations.len(), 2);
        c.release(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        )
        .unwrap();

        let overfull = req(
            "overfull-bundle",
            vec![capacity("worker-a", 2, 3), capacity("worker-b", 2, 3)],
        );
        let plan = c.plan(&overfull).unwrap();
        assert!(!plan.available);
        assert!(plan.proposed.is_empty());
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].code, "resource_contention");
        assert_eq!(plan.conflicts[0].requested, "workers:4/3");
        assert_eq!(
            conflict_code(c.acquire(&overfull).unwrap_err()),
            "resource_contention"
        );
        assert!(
            c.list(true)
                .unwrap()
                .iter()
                .all(|lease| lease.request_id != "overfull-bundle"),
            "an overfull capacity bundle must not create a lease"
        );
    }

    #[test]
    fn same_pool_capacity_requirements_must_use_one_limit() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let request = req(
            "mixed-limit-bundle",
            vec![
                HostResourceRequirement {
                    key: "worker-a".into(),
                    resource: HostResourceKind::Capacity {
                        pool: "workers".into(),
                        units: 1,
                        limit: 4,
                    },
                },
                HostResourceRequirement {
                    key: "worker-b".into(),
                    resource: HostResourceKind::Capacity {
                        pool: "workers".into(),
                        units: 1,
                        limit: 5,
                    },
                },
            ],
        );

        let plan = c.plan(&request).unwrap();
        assert!(!plan.available);
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].code, "capacity_policy_mismatch");
        assert!(plan.conflicts[0].owning_lease.is_none());
        let explanation = c.explain(&request).unwrap();
        assert!(
            explanation.blockers[0]
                .recovery
                .contains("throughout the request")
        );
        assert!(!explanation.wait.waitable);
        assert_eq!(explanation.wait.reason, "capacity_policy_mismatch");
        assert!(explanation.wait.action.contains("throughout the request"));
        assert_eq!(
            conflict_code(c.acquire(&request).unwrap_err()),
            "capacity_policy_mismatch"
        );
        assert!(
            c.list(true)
                .unwrap()
                .iter()
                .all(|lease| lease.request_id != "mixed-limit-bundle"),
            "a capacity-policy mismatch must not create a lease"
        );
    }

    #[test]
    fn same_pool_capacity_total_does_not_wrap() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let request = req(
            "overflowing-bundle",
            vec![
                HostResourceRequirement {
                    key: "worker-a".into(),
                    resource: HostResourceKind::Capacity {
                        pool: "workers".into(),
                        units: u32::MAX,
                        limit: u32::MAX,
                    },
                },
                HostResourceRequirement {
                    key: "worker-b".into(),
                    resource: HostResourceKind::Capacity {
                        pool: "workers".into(),
                        units: 1,
                        limit: u32::MAX,
                    },
                },
            ],
        );

        let plan = c.plan(&request).unwrap();
        assert!(!plan.available);
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].code, "resource_contention");
        assert_eq!(plan.conflicts[0].requested, "workers:4294967296/4294967295");
        assert_eq!(
            conflict_code(c.acquire(&request).unwrap_err()),
            "resource_contention"
        );
        assert!(
            c.list(true)
                .unwrap()
                .iter()
                .all(|lease| lease.request_id != "overflowing-bundle"),
            "a capacity total greater than u32::MAX must not wrap into admission"
        );
    }

    #[test]
    fn supervised_capacity_hold_survives_dead_supervisor_before_terminal_proof() {
        let t = tempfile::tempdir().unwrap();
        let mut coordinator = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let dead_holder = reaped_pid();
        let request = capacity_request("supervised", "workers", 1, 1, Some(dead_holder));
        let grant = coordinator.acquire_inner(&request, true).unwrap();

        let report = coordinator.reap_dead_holders().unwrap();
        assert_eq!(report.reclaimed_capacity_units, 0);
        assert!(report.leases.is_empty());
        let lease = coordinator.list(false).unwrap().remove(0);
        assert_eq!(lease.holder_pid, Some(dead_holder));
        assert_eq!(lease.state, HostLeaseState::Quarantined);
        assert!(matches!(
            coordinator.acquire(&capacity_request(
                "contender",
                "workers",
                1,
                1,
                Some(std::process::id())
            )),
            Err(HostResourceError::Conflict { .. })
        ));
        assert!(matches!(
            coordinator.release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token
            ),
            Err(HostResourceError::InvalidRequest(_))
        ));

        let reconciled = coordinator
            .reconcile(&grant.lease.lease_id, grant.lease.generation)
            .unwrap();
        assert_eq!(reconciled.state, HostLeaseState::Released);
        assert!(
            coordinator
                .plan(&capacity_request(
                    "after-reconcile",
                    "workers",
                    1,
                    1,
                    Some(std::process::id())
                ))
                .unwrap()
                .available
        );
    }

    #[cfg(unix)]
    #[test]
    fn supervised_run_does_not_prepare_or_spawn_before_capacity_is_acquired() {
        let t = tempfile::tempdir().unwrap();
        let mut coordinator = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        coordinator
            .acquire(&capacity_request(
                "capacity-owner",
                "workers",
                1,
                1,
                Some(std::process::id()),
            ))
            .unwrap();

        let child_marker = t.path().join("child-started");
        let mut environment_prepared = false;
        let result = coordinator.run_supervised_with_environment(
            &capacity_request(
                "blocked-supervised-run",
                "workers",
                1,
                1,
                Some(std::process::id()),
            ),
            std::time::Duration::ZERO,
            &["sh".into(), "-c".into(), "touch \"$CHILD_MARKER\"".into()],
            None,
            t.path(),
            |_| {
                environment_prepared = true;
                Ok(BTreeMap::from([(
                    "CHILD_MARKER".into(),
                    child_marker.to_string_lossy().into_owned(),
                )]))
            },
            |_| {},
        );

        assert!(matches!(
            result,
            Err(HostResourceRunError::Resource(
                HostResourceError::Conflict { .. }
            ))
        ));
        assert!(!environment_prepared);
        assert!(!child_marker.exists());
    }

    // Include SQLite's connection-local write counter as well as the public
    // inventory: recovery may not silently renew or mutate a hidden hold.
    fn recovery_snapshot(
        coordinator: &HostResourceCoordinator,
    ) -> (Vec<HostResourceLease>, i64, i64, i64) {
        (
            coordinator.list(true).unwrap(),
            coordinator
                .conn
                .query_row("SELECT value FROM meta WHERE key='generation'", [], |row| {
                    row.get(0)
                })
                .unwrap(),
            coordinator
                .conn
                .query_row(
                    "SELECT count(*) FROM resource_supervised_capacity_holds",
                    [],
                    |row| row.get(0),
                )
                .unwrap(),
            coordinator
                .conn
                .query_row("SELECT total_changes()", [], |row| row.get(0))
                .unwrap(),
        )
    }

    #[test]
    fn supervised_recovery_after_reopen_preserves_the_exact_grant_without_writes() {
        let t = tempfile::tempdir().unwrap();
        let database = t.path().join("h.db");
        let request = capacity_request(
            "recover-supervised",
            "workers",
            1,
            1,
            Some(std::process::id()),
        );
        let mut coordinator = HostResourceCoordinator::open(&database).unwrap();
        let original = coordinator.acquire_inner(&request, true).unwrap();
        drop(coordinator);
        let coordinator = HostResourceCoordinator::open(&database).unwrap();
        let before = recovery_snapshot(&coordinator);
        for _ in 0..2 {
            assert_eq!(
                coordinator.recover_request_inner(&request, true).unwrap(),
                Some(original.clone())
            );
            assert_eq!(recovery_snapshot(&coordinator), before);
        }
    }

    #[test]
    fn supervised_recovery_requires_the_exact_request_and_digest_mode() {
        let t = tempfile::tempdir().unwrap();
        let mut coordinator = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let request =
            capacity_request("supervised-mode", "workers", 1, 2, Some(std::process::id()));
        coordinator.acquire_inner(&request, true).unwrap();
        let before = recovery_snapshot(&coordinator);
        assert!(matches!(
            coordinator.recover_request(&request),
            Err(HostResourceError::IdempotencyMismatch(_))
        ));
        let mut changed = request.clone();
        changed.run_id.push_str("-different");
        assert!(matches!(
            coordinator.recover_request_inner(&changed, true),
            Err(HostResourceError::IdempotencyMismatch(_))
        ));
        assert_eq!(recovery_snapshot(&coordinator), before);
        assert!(matches!(
            coordinator.acquire_inner(&request, true),
            Err(HostResourceError::RequestNotActive { .. })
        ));

        let ordinary = capacity_request(
            "ordinary-mode",
            "ordinary-workers",
            1,
            2,
            Some(std::process::id()),
        );
        let grant = coordinator.acquire(&ordinary).unwrap();
        let before = recovery_snapshot(&coordinator);
        assert!(matches!(
            coordinator.recover_request_inner(&ordinary, true),
            Err(HostResourceError::IdempotencyMismatch(_))
        ));
        assert_eq!(coordinator.recover_request(&ordinary).unwrap(), Some(grant));
        assert_eq!(recovery_snapshot(&coordinator), before);
    }

    #[test]
    fn supervised_recovery_of_a_missing_request_does_not_acquire() {
        let t = tempfile::tempdir().unwrap();
        let coordinator = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let request = capacity_request("never-admitted", "workers", 1, 1, None);
        let before = recovery_snapshot(&coordinator);
        assert_eq!(
            coordinator.recover_request_inner(&request, true).unwrap(),
            None
        );
        assert_eq!(coordinator.recover_request(&request).unwrap(), None);
        assert_eq!(recovery_snapshot(&coordinator), before);
    }

    #[test]
    fn supervised_recovery_preserves_quarantine_and_does_not_release_capacity() {
        let t = tempfile::tempdir().unwrap();
        let mut coordinator = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let request = capacity_request(
            "quarantined-recovery",
            "workers",
            1,
            1,
            Some(std::process::id()),
        );
        let original = coordinator.acquire_inner(&request, true).unwrap();
        coordinator
            .quarantine(
                &original.lease.lease_id,
                original.lease.generation,
                &original.ownership_token,
            )
            .unwrap();
        let before = recovery_snapshot(&coordinator);
        let recovered = coordinator
            .recover_request_inner(&request, true)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.lease.state, HostLeaseState::Quarantined);
        assert_eq!(recovered.lease.lease_id, original.lease.lease_id);
        assert_eq!(recovered.lease.generation, original.lease.generation);
        assert_eq!(recovered.ownership_token, original.ownership_token);
        assert_eq!(recovery_snapshot(&coordinator), before);
        assert!(matches!(
            coordinator.release(
                &recovered.lease.lease_id,
                recovered.lease.generation,
                &recovered.ownership_token
            ),
            Err(HostResourceError::InvalidRequest(_))
        ));
        let contender = capacity_request(
            "recovery-contender",
            "workers",
            1,
            1,
            Some(std::process::id()),
        );
        assert!(!coordinator.plan(&contender).unwrap().available);
    }

    #[test]
    fn supervised_recovery_returns_terminal_history_without_reactivating_it() {
        let t = tempfile::tempdir().unwrap();
        let mut coordinator = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let request = capacity_request(
            "terminal-recovery",
            "workers",
            1,
            1,
            Some(std::process::id()),
        );
        let original = coordinator.acquire_inner(&request, true).unwrap();
        let terminal = coordinator
            .release_supervised_verified(
                &original.lease.lease_id,
                original.lease.generation,
                &original.ownership_token,
            )
            .unwrap();
        let before = recovery_snapshot(&coordinator);
        let recovered = coordinator
            .recover_request_inner(&request, true)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.lease, terminal);
        assert_eq!(recovered.lease.state, HostLeaseState::Released);
        assert_eq!(recovered.ownership_token, original.ownership_token);
        assert_eq!(recovery_snapshot(&coordinator), before);
        assert!(matches!(
            coordinator.acquire_inner(&request, true),
            Err(HostResourceError::RequestNotActive { .. })
        ));
    }

    #[test]
    fn supervised_recovery_refuses_a_missing_or_wrong_generation_hold() {
        for wrong_generation in [false, true] {
            let t = tempfile::tempdir().unwrap();
            let mut coordinator = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
            let request = capacity_request(
                "corrupt-recovery",
                "workers",
                1,
                1,
                Some(std::process::id()),
            );
            let grant = coordinator.acquire_inner(&request, true).unwrap();
            // Deliberately corrupt only this isolated fixture's private hold.
            if wrong_generation {
                coordinator.conn.execute("UPDATE resource_supervised_capacity_holds SET generation=generation+1 WHERE lease_id=?1", [&grant.lease.lease_id]).unwrap();
            } else {
                coordinator
                    .conn
                    .execute(
                        "DELETE FROM resource_supervised_capacity_holds WHERE lease_id=?1",
                        [&grant.lease.lease_id],
                    )
                    .unwrap();
            }
            let before = recovery_snapshot(&coordinator);
            assert!(
                matches!(coordinator.recover_request_inner(&request, true), Err(HostResourceError::InvalidRequest(message)) if message == "supervised recovery requires its generation-bound hold")
            );
            assert_eq!(recovery_snapshot(&coordinator), before);
        }
    }

    #[test]
    fn supervised_request_id_cannot_be_replayed_or_cross_modes() {
        let t = tempfile::tempdir().unwrap();
        let mut coordinator = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let supervised = capacity_request(
            "supervised-request",
            "supervised-pool",
            1,
            2,
            Some(std::process::id()),
        );
        coordinator.acquire_inner(&supervised, true).unwrap();
        assert!(matches!(
            coordinator.acquire_inner(&supervised, true),
            Err(HostResourceError::RequestNotActive { .. })
        ));
        assert!(matches!(
            coordinator.acquire(&supervised),
            Err(HostResourceError::IdempotencyMismatch(_))
        ));

        let generic = capacity_request(
            "generic-request",
            "generic-pool",
            1,
            2,
            Some(std::process::id()),
        );
        coordinator.acquire(&generic).unwrap();
        assert!(matches!(
            coordinator.acquire_inner(&generic, true),
            Err(HostResourceError::IdempotencyMismatch(_))
        ));
    }

    #[test]
    fn reap_preview_preserves_known_legacy_managed_registries() {
        for version in 1..=3 {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("legacy.db");
            create_managed_schema_fixture(&db, version);
            let mut writer = Connection::open(&db).unwrap();
            insert_legacy_v1_capacity(&mut writer, "preview", "preview", 1, 4, Some(reaped_pid()))
                .unwrap();
            let before = migration_snapshot(&writer, version);
            drop(writer);
            let reader = HostResourceCoordinator::open_read_only(&db).unwrap();
            let leases = reader.list(true).unwrap();
            assert!(
                !leases.is_empty(),
                "legacy fixture must have retained leases"
            );
            let preview = reader.preview_dead_holders().unwrap();
            assert_eq!(preview.candidates.len(), 1);
            assert_eq!(preview.would_reclaim_capacity_units, 1);
            assert_eq!(reader.list(true).unwrap(), leases);
            assert_eq!(migration_snapshot(&reader.conn, version), before);
        }
    }

    #[test]
    fn reap_preview_refuses_retained_v1_host_store_without_upgrade() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("host-v1.db");
        let mut writer = Connection::open(&db).unwrap();
        writer.execute_batch(SCHEMA).unwrap();
        insert_legacy_v1_capacity(&mut writer, "preview", "preview", 1, 4, Some(reaped_pid()))
            .unwrap();
        drop(writer);
        let reader = HostResourceCoordinator::open_read_only(&db).unwrap();
        let before = reader.list(true).unwrap();
        assert!(reader.preview_dead_holders().is_err());
        assert_eq!(reader.list(true).unwrap(), before);
        assert_eq!(store_schema_version(&reader.conn).unwrap(), 1);
    }

    #[test]
    fn reap_preview_keeps_unsettled_preparation_capacity_fenced() {
        for version in [3, 4] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("unsettled.db");
            create_managed_schema_fixture(&db, 2);
            let mut writer = Connection::open(&db).unwrap();
            let dead_holder = reaped_pid();
            // Capacity exists before preparation creates the producer fence.
            writer
                .execute(
                    "INSERT INTO resource_leases
                 (lease_id,request_id,request_digest,repository,worktree_fingerprint,run_id,
                  generation,ownership_token,state,holder_pid,created_at,expires_at,updated_at)
                 VALUES ('legacy-lease','legacy-request','digest','migration-repo','fixture','run',
                         7,'token','active',?1,1,9223372036854775807,1)",
                    [dead_holder],
                )
                .unwrap();
            writer
                .execute(
                    "INSERT INTO resource_allocations
                 (lease_id,resource_key,kind,value,units,capacity_limit)
                 VALUES ('legacy-lease','worker','capacity','preview',1,4)",
                    [],
                )
                .unwrap();
            writer
                .execute_batch(MANAGED_STORAGE_PRODUCER_SCHEMA)
                .unwrap();
            seed_v3_managed_rows(&writer);
            writer
                .execute(
                    "UPDATE managed_storage_meta SET value=3 WHERE key='schema_version'",
                    [],
                )
                .unwrap();
            if version == 4 {
                let tx = writer
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                migrate_managed_storage_schema(&tx).unwrap();
                tx.commit().unwrap();
            }
            drop(writer);
            let reader = HostResourceCoordinator::open_read_only(&db).unwrap();
            let before = reader.list(true).unwrap();
            assert_eq!(
                before.len(),
                1,
                "fixture must retain producer-linked capacity"
            );
            assert_eq!(before[0].allocations.len(), 1);
            let preview = reader.preview_dead_holders().unwrap();
            assert!(
                preview.candidates.is_empty(),
                "v{version}: unsettled preparation is not reclaimable"
            );
            assert_eq!(preview.would_reclaim_capacity_units, 0);
            assert_eq!(reader.list(true).unwrap(), before);
        }
    }

    #[test]
    fn legacy_v1_capacity_reclamation_survives_v2_migration_and_readonly() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("legacy.db");
        let dead_holder = reaped_pid();
        let mut legacy = Connection::open(&path).unwrap();
        legacy.execute_batch(SCHEMA).unwrap();
        insert_legacy_v1_capacity(
            &mut legacy,
            "legacy-dead",
            "legacy-pool",
            1,
            4,
            Some(dead_holder),
        )
        .unwrap();
        legacy
            .execute(
                "INSERT INTO resource_allocations \
                 (lease_id,resource_key,kind,value,units,capacity_limit) \
                 VALUES ('legacy-legacy-dead','database','exclusive_key','legacy-db',NULL,NULL)",
                [],
            )
            .unwrap();
        drop(legacy);

        let readonly = HostResourceCoordinator::open_read_only(&path).unwrap();
        assert!(
            readonly
                .plan(&capacity_request(
                    "readonly-v1-plan",
                    "legacy-pool",
                    1,
                    4,
                    Some(std::process::id())
                ))
                .unwrap()
                .available
        );
        drop(readonly);

        let mut coordinator = HostResourceCoordinator::open(&path).unwrap();
        assert_eq!(store_schema_version(&coordinator.conn).unwrap(), 2);
        let before = coordinator.list(false).unwrap().remove(0);
        assert_eq!(before.holder_pid, Some(dead_holder));
        assert_eq!(before.request_id, "legacy-dead");
        assert_eq!(before.generation, 1);
        let report = coordinator.reap_dead_holders().unwrap();
        assert_eq!(report.reclaimed_capacity_units, 1);
        assert_eq!(report.retained_quarantined_leases, 1);
        let after = coordinator.list(false).unwrap().remove(0);
        assert_eq!(after.holder_pid, Some(dead_holder));
        assert_eq!(after.state, HostLeaseState::Quarantined);
        assert_eq!(after.allocations.len(), 1);
        assert_eq!(after.allocations[0].kind, "exclusive_key");
        drop(coordinator);

        let newer = Connection::open(&path).unwrap();
        newer
            .execute("UPDATE meta SET value=3 WHERE key='schema_version'", [])
            .unwrap();
        drop(newer);
        assert!(matches!(
            HostResourceCoordinator::open_read_only(&path),
            Err(HostResourceError::InvalidRequest(message)) if message.contains("unsupported host schema 3")
        ));
    }

    #[test]
    fn old_open_v1_connection_cannot_release_or_reap_supervised_capacity() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("old-open.db");
        let mut old_connection = Connection::open(&path).unwrap();
        old_connection.execute_batch(SCHEMA).unwrap();
        old_connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        old_connection
            .pragma_update(None, "journal_mode", "wal")
            .unwrap();

        let mut coordinator = HostResourceCoordinator::open(&path).unwrap();
        let dead_holder = reaped_pid();
        let request = capacity_request("held", "guarded-pool", 1, 2, Some(dead_holder));
        let grant = coordinator.acquire_inner(&request, true).unwrap();
        let (stored_pid, stored_units): (i64, i64) = old_connection
            .query_row(
                "SELECT l.holder_pid,a.units FROM resource_leases l \
                 JOIN resource_allocations a ON a.lease_id=l.lease_id \
                 WHERE l.lease_id=?1 AND a.kind='capacity'",
                [&grant.lease.lease_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored_pid as u32, dead_holder);
        assert_eq!(stored_units, 1);
        assert!(holder_process_is_gone(stored_pid));
        // The already-open v1 planner would omit this dead-holder capacity.
        assert_eq!(
            if holder_process_is_gone(stored_pid) {
                0
            } else {
                stored_units
            },
            0
        );

        {
            let tx = old_connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            tx.execute(
                "INSERT INTO resource_reap_observations(lease_id,generation,reaped_at) \
                 VALUES (?1,?2,?3) ON CONFLICT(lease_id) DO NOTHING",
                params![
                    grant.lease.lease_id,
                    grant.lease.generation as i64,
                    now_ms()
                ],
            )
            .unwrap();
            assert!(
                tx.execute(
                    "DELETE FROM resource_allocations WHERE lease_id=?1 AND kind='capacity'",
                    [&grant.lease.lease_id],
                )
                .is_err()
            );
            tx.rollback().unwrap();
        }
        assert!(
            old_connection
                .execute(
                    "DELETE FROM resource_allocations WHERE lease_id=?1 AND kind='capacity'",
                    [&grant.lease.lease_id],
                )
                .is_err()
        );
        assert!(
            old_connection
                .execute(
                    "UPDATE resource_leases SET state='released' WHERE lease_id=?1",
                    [&grant.lease.lease_id],
                )
                .is_err()
        );
        assert!(
            old_connection
                .execute(
                    "UPDATE resource_allocations SET units=0 WHERE lease_id=?1 AND kind='capacity'",
                    [&grant.lease.lease_id],
                )
                .is_err()
        );

        // Disable SQLite's foreign-key check for this statement so the
        // generation-bound trigger, rather than the hold table's FK, proves
        // that an already-open v1 writer cannot delete the held lease.
        old_connection
            .pragma_update(None, "foreign_keys", "OFF")
            .unwrap();
        assert!(
            old_connection
                .execute(
                    "DELETE FROM resource_leases WHERE lease_id=?1",
                    [&grant.lease.lease_id],
                )
                .is_err()
        );
        old_connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();

        let legacy_second = capacity_request(
            "legacy-second",
            "guarded-pool",
            1,
            2,
            Some(std::process::id()),
        );
        insert_legacy_v1_capacity(
            &mut old_connection,
            &legacy_second.request_id,
            "guarded-pool",
            1,
            2,
            legacy_second.holder_pid,
        )
        .unwrap();
        assert!(
            insert_legacy_v1_capacity(
                &mut old_connection,
                "legacy-third",
                "guarded-pool",
                1,
                2,
                Some(std::process::id()),
            )
            .is_err()
        );

        let report = coordinator.reap_dead_holders().unwrap();
        assert_eq!(report.reclaimed_capacity_units, 0);
        assert!(matches!(
            coordinator.acquire(&capacity_request(
                "new-contender",
                "guarded-pool",
                1,
                2,
                Some(std::process::id())
            )),
            Err(HostResourceError::Conflict { .. })
        ));
        assert_eq!(
            old_connection
                .query_row(
                    "SELECT COUNT(*) FROM resource_allocations WHERE lease_id=?1 AND kind='capacity'",
                    [&grant.lease.lease_id],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn authority_loss_kills_a_leader_that_ignores_term_within_the_grace() {
        let t = tempfile::tempdir().unwrap();
        let database = t.path().join("h.db");
        let mut coordinator = HostResourceCoordinator::open(&database).unwrap();
        let request = req("authority-loss", vec![exclusive("authority-loss")]);
        let mut grant = coordinator.acquire(&request).unwrap();
        grant.lease = coordinator
            .quarantine(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
            .unwrap();

        let ready_path = t.path().join("term-handler-ready");
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            "trap '' TERM; : > \"$1\"; exec sleep 3".to_string(),
            "authority-loss-child".to_string(),
            ready_path.display().to_string(),
        ];
        let environment = BTreeMap::new();
        let mut child = spawn_resource_process(&command, t.path(), &grant, &environment).unwrap();
        let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !ready_path.exists() && std::time::Instant::now() < ready_deadline {
            if child.child.try_wait().unwrap().is_some() {
                panic!("owned test child exited before installing its TERM handler");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !ready_path.exists() {
            if child.child.try_wait().unwrap().is_none() {
                let kill_result = signal_unreaped_process_group(&child, libc::SIGKILL);
                let reap_result = wait_for_child_exit(&mut child, AUTHORITY_LOSS_KILL_GRACE);
                assert!(
                    kill_result.is_ok(),
                    "failed to kill the owned test process group"
                );
                assert!(
                    matches!(&reap_result, Ok(Some(_))),
                    "failed to reap the owned test child after readiness timeout: {reap_result:?}"
                );
            }
            panic!("owned test child did not install its TERM handler before the deadline");
        }
        let started = std::time::Instant::now();
        let mut events = Vec::new();
        let status = terminate_after_authority_loss(&mut child, &mut |message| {
            events.push(message.to_string())
        })
        .unwrap()
        .expect("the owned leader must be reaped within the bounded TERM/KILL waits");
        let completion = process_completion(&child, status);

        assert!(
            started.elapsed()
                < AUTHORITY_LOSS_TERM_GRACE
                    + AUTHORITY_LOSS_KILL_GRACE
                    + std::time::Duration::from_secs(2),
            "authority-loss termination exceeded its bounded waits"
        );
        assert_eq!(exit_signal(&completion.status), Some(libc::SIGKILL));
        assert_eq!(completion.process_group, ProcessGroupObservation::Absent);
        assert_eq!(grant.lease.state, HostLeaseState::Quarantined);
        assert!(events.iter().any(|event| event.contains("SIGKILL")));
        assert_eq!(
            coordinator.list(false).unwrap()[0].state,
            HostLeaseState::Quarantined
        );
    }

    #[test]
    fn environment_failure_records_finalization_and_keeps_capacity_quarantined() {
        let t = tempfile::tempdir().unwrap();
        let mut coordinator = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let request = req("environment-failure-phases", vec![exclusive("environment")]);
        let mut phases = Vec::new();
        let error = coordinator
            .run_supervised_with_environment_observed(
                &request,
                std::time::Duration::ZERO,
                &[t.path()
                    .join("nonexistent-child")
                    .to_string_lossy()
                    .into_owned()],
                None,
                t.path(),
                |_| Err(std::io::Error::other("deliberate environment failure")),
                |_| {},
                |phase, _| phases.push(phase),
            )
            .unwrap_err();
        assert!(matches!(error, HostResourceRunError::Environment(_)));
        assert_eq!(
            phases,
            [
                "resource_acquisition",
                "environment_preparation",
                "lease_finalization"
            ]
        );
        assert_eq!(
            coordinator.list(false).unwrap()[0].state,
            HostLeaseState::Quarantined
        );
    }

    #[test]
    fn spawn_failure_records_finalization_and_keeps_capacity_quarantined() {
        let t = tempfile::tempdir().unwrap();
        let mut coordinator = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let request = req("spawn-failure-phases", vec![exclusive("spawn")]);
        let mut phases = Vec::new();
        let error = coordinator
            .run_supervised_with_environment_observed(
                &request,
                std::time::Duration::ZERO,
                &[t.path()
                    .join("nonexistent-child")
                    .to_string_lossy()
                    .into_owned()],
                None,
                t.path(),
                |_| Ok(BTreeMap::new()),
                |_| {},
                |phase, _| phases.push(phase),
            )
            .unwrap_err();
        assert!(matches!(error, HostResourceRunError::Spawn { .. }));
        assert_eq!(
            phases,
            [
                "resource_acquisition",
                "environment_preparation",
                "child_spawn",
                "lease_finalization"
            ]
        );
        assert_eq!(
            coordinator.list(false).unwrap()[0].state,
            HostLeaseState::Quarantined
        );
    }

    #[test]
    fn unconfirmed_authority_loss_termination_quarantines_without_a_status() {
        let t = tempfile::tempdir().unwrap();
        let mut coordinator = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let request = req(
            "unconfirmed-authority-loss",
            vec![exclusive("unknown-status")],
        );
        let mut grant = coordinator.acquire(&request).unwrap();
        let mut events = Vec::new();

        let mut phases = Vec::new();
        let error = quarantine_unconfirmed_termination(
            &mut coordinator,
            &mut grant,
            "the child status remained unavailable after bounded TERM/KILL waits".into(),
            &mut |message| events.push(message.to_string()),
            &mut |phase, _| phases.push(phase),
        );
        assert_eq!(phases, ["lease_finalization"]);

        assert!(matches!(
            error,
            HostResourceRunError::TerminationUnconfirmed {
                ref lease_id,
                ref lease_state,
                ..
            } if lease_id == &grant.lease.lease_id && lease_state == "quarantined"
        ));
        assert_eq!(grant.lease.state, HostLeaseState::Quarantined);
        assert_eq!(
            coordinator.list(false).unwrap()[0].state,
            HostLeaseState::Quarantined
        );
        assert!(
            events
                .iter()
                .any(|event| event.contains("status unavailable"))
        );
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

    fn capacity_request(
        id: &str,
        pool: &str,
        units: u32,
        limit: u32,
        holder_pid: Option<u32>,
    ) -> HostResourceRequest {
        let mut request = req(
            id,
            vec![HostResourceRequirement {
                key: "workers".into(),
                resource: HostResourceKind::Capacity {
                    pool: pool.into(),
                    units,
                    limit,
                },
            }],
        );
        request.holder_pid = holder_pid;
        request
    }

    fn insert_legacy_v1_capacity(
        conn: &mut Connection,
        request_id: &str,
        pool: &str,
        units: u32,
        limit: u32,
        holder_pid: Option<u32>,
    ) -> Result<(), rusqlite::Error> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let generation: i64 = tx.query_row(
            "UPDATE meta SET value=value+1 WHERE key='generation' RETURNING value",
            [],
            |row| row.get(0),
        )?;
        let lease_id = format!("legacy-{request_id}");
        tx.execute(
            "INSERT INTO resource_leases \
             (lease_id,request_id,request_digest,repository,worktree_fingerprint,run_id, \
              generation,ownership_token,state,holder_pid,created_at,expires_at,updated_at) \
             VALUES (?1,?2,'legacy-digest','owner/repo','legacy-fingerprint',?2,?3, \
                     'legacy-token','active',?4,1,9223372036854775807,1)",
            params![lease_id, request_id, generation, holder_pid.map(i64::from)],
        )?;
        tx.execute(
            "INSERT INTO resource_allocations \
             (lease_id,resource_key,kind,value,units,capacity_limit) \
             VALUES (?1,'workers','capacity',?2,?3,?4)",
            params![lease_id, pool, i64::from(units), i64::from(limit)],
        )?;
        tx.commit()
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
    fn dead_holder_still_reserves_its_named_resources_without_waiting() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let mut leaked = req("leaked", vec![exclusive("shared")]);
        leaked.ttl_seconds = 3_600;
        leaked.holder_pid = Some(reaped_pid());
        let leaked = c.acquire(&leaked).unwrap();

        let explanation = c
            .explain(&req("explained", vec![exclusive("shared")]))
            .unwrap();
        assert!(!explanation.wait.waitable);
        assert_eq!(explanation.wait.reason, ORPHANED_HOLDER_CONFLICT_CODE);

        let mut conflict_callbacks = 0;
        let error = c
            .acquire_with_wait(
                &req("next", vec![exclusive("shared")]),
                std::time::Duration::from_secs(30),
                |_| conflict_callbacks += 1,
            )
            .unwrap_err();
        let HostResourceError::Conflict {
            code,
            message,
            conflicts,
        } = error
        else {
            panic!("a dead holder's exclusive key must stay reserved");
        };
        assert_eq!(code, ORPHANED_HOLDER_CONFLICT_CODE);
        assert_eq!(
            conflict_callbacks, 0,
            "a dead holder must not enter the wait loop"
        );
        assert_eq!(conflicts.len(), 1);
        assert_eq!(
            conflicts[0].owning_lease.as_deref(),
            Some(leaked.lease.lease_id.as_str())
        );
        assert!(
            message.contains("holder process is gone")
                && message.contains(&format!(
                    "aethyme broker advanced resources reconcile {} --confirm {}",
                    leaked.lease.lease_id, leaked.lease.generation
                )),
            "conflict must identify the exact reconciliation action: {message}"
        );
        drop(c);
        let c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let leases = c.list(false).unwrap();
        assert_eq!(
            leases.len(),
            1,
            "the rejected request must not create a lease"
        );
        let retained = leases
            .into_iter()
            .find(|lease| lease.lease_id == leaked.lease.lease_id)
            .unwrap();
        assert_eq!(retained.state, HostLeaseState::Quarantined);
        assert_eq!(retained.allocations.len(), 1);
        assert_eq!(retained.allocations[0].value, "shared");
    }

    #[test]
    fn orphaned_exclusive_refusal_commits_safe_capacity_reclaim_only() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("h.db");
        let mut c = HostResourceCoordinator::open(&path).unwrap();
        let mut dead_owner = capacity_request("dead-owner", "workers", 1, 1, Some(reaped_pid()));
        dead_owner.resources.push(exclusive("shared"));
        let dead_owner = c.acquire(&dead_owner).unwrap();
        assert_eq!(dead_owner.lease.allocations.len(), 2);

        let mut rejected = capacity_request("rejected", "workers", 1, 1, None);
        rejected.resources.push(exclusive("shared"));
        let error = c.acquire(&rejected).unwrap_err();
        let HostResourceError::Conflict { code, .. } = error else {
            panic!("the dead exclusive key must refuse the bundle");
        };
        assert_eq!(code, ORPHANED_HOLDER_CONFLICT_CODE);

        drop(c);
        let c = HostResourceCoordinator::open(&path).unwrap();
        let leases = c.list(true).unwrap();
        assert_eq!(
            leases.len(),
            1,
            "the rejected bundle must not create a lease"
        );
        let retained = &leases[0];
        assert_eq!(retained.lease_id, dead_owner.lease.lease_id);
        assert_eq!(retained.state, HostLeaseState::Quarantined);
        assert_eq!(retained.allocations.len(), 1);
        assert_eq!(retained.allocations[0].kind, "exclusive_key");
        assert_eq!(retained.allocations[0].value, "shared");
    }

    #[test]
    fn capacity_policy_mismatch_does_not_commit_orphan_quarantine() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        c.acquire(&capacity_request(
            "live-capacity",
            "workers",
            1,
            2,
            Some(std::process::id()),
        ))
        .unwrap();
        let mut dead_exclusive = req("dead-exclusive", vec![exclusive("shared")]);
        dead_exclusive.holder_pid = Some(reaped_pid());
        let dead_exclusive = c.acquire(&dead_exclusive).unwrap();

        let mut request = req(
            "mismatched-bundle",
            vec![
                exclusive("shared"),
                HostResourceRequirement {
                    key: "workers".into(),
                    resource: HostResourceKind::Capacity {
                        pool: "workers".into(),
                        units: 1,
                        limit: 3,
                    },
                },
            ],
        );
        request.holder_pid = Some(std::process::id());
        let error = c.acquire(&request).unwrap_err();
        let HostResourceError::Conflict {
            code, conflicts, ..
        } = error
        else {
            panic!("the pool policy mismatch must refuse the bundle");
        };
        assert_eq!(code, "capacity_policy_mismatch");
        assert!(conflicts.iter().any(|conflict| {
            conflict.code == ORPHANED_HOLDER_CONFLICT_CODE
                && conflict.owning_lease.as_deref() == Some(dead_exclusive.lease.lease_id.as_str())
        }));
        let retained = c
            .list(false)
            .unwrap()
            .into_iter()
            .find(|lease| lease.lease_id == dead_exclusive.lease.lease_id)
            .unwrap();
        assert_eq!(retained.state, HostLeaseState::Active);
        assert_eq!(retained.allocations.len(), 1);
        assert_eq!(retained.allocations[0].value, "shared");
    }

    #[test]
    fn dead_holder_conflict_precedes_waitable_conflicts_in_a_bundle() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let mut dead_key = req("dead-key", vec![exclusive("shared")]);
        dead_key.holder_pid = Some(reaped_pid());
        let dead_key = c.acquire(&dead_key).unwrap();
        c.acquire(&whole_pool("live-capacity")).unwrap();

        let mut request = req(
            "bundle",
            vec![
                exclusive("shared"),
                HostResourceRequirement {
                    key: "host-work".into(),
                    resource: HostResourceKind::Capacity {
                        pool: "host-work".into(),
                        units: 1,
                        limit: 4,
                    },
                },
            ],
        );
        request.holder_pid = Some(std::process::id());
        let mut conflict_callbacks = 0;
        let error = c
            .acquire_with_wait(&request, std::time::Duration::from_secs(30), |_| {
                conflict_callbacks += 1
            })
            .unwrap_err();
        let HostResourceError::Conflict {
            code, conflicts, ..
        } = error
        else {
            panic!("the bundle must remain blocked by both existing leases");
        };
        assert_eq!(code, ORPHANED_HOLDER_CONFLICT_CODE);
        assert_eq!(conflict_callbacks, 0);
        assert!(conflicts.iter().any(|conflict| {
            conflict.code == ORPHANED_HOLDER_CONFLICT_CODE
                && conflict.owning_lease.as_deref() == Some(dead_key.lease.lease_id.as_str())
        }));
        assert!(conflicts.iter().any(|conflict| {
            conflict.kind == "capacity" && conflict.code == "resource_contention"
        }));
    }

    #[test]
    fn dead_capacity_and_tcp_holders_do_not_make_a_bundle_unwaitable() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();

        let dead_capacity =
            capacity_request("supervised-dead", "supervised", 1, 4, Some(reaped_pid()));
        c.acquire_inner(&dead_capacity, true).unwrap();
        c.acquire(&capacity_request(
            "live-capacity",
            "supervised",
            3,
            4,
            Some(std::process::id()),
        ))
        .unwrap();
        let next_capacity = capacity_request(
            "next-capacity",
            "supervised",
            1,
            4,
            Some(std::process::id()),
        );
        let capacity_explanation = c.explain(&next_capacity).unwrap();
        assert!(capacity_explanation.wait.waitable);
        assert_eq!(capacity_explanation.wait.reason, "resource_contention");
        let mut capacity_callbacks = 0;
        let capacity_error = c
            .acquire_with_wait(
                &next_capacity,
                std::time::Duration::from_millis(250),
                |_| capacity_callbacks += 1,
            )
            .unwrap_err();
        assert_eq!(conflict_code(capacity_error), "resource_contention");
        assert!(
            capacity_callbacks > 0,
            "the live capacity holder may release enough units for this request"
        );

        let (dead_port, live_port) = adjacent_bindable_ports();
        let mut dead_port_request = req(
            "dead-port",
            vec![HostResourceRequirement {
                key: "port".into(),
                resource: HostResourceKind::TcpPort {
                    start: dead_port,
                    end: dead_port,
                },
            }],
        );
        dead_port_request.holder_pid = Some(reaped_pid());
        let dead_port_lease = c.acquire(&dead_port_request).unwrap();
        let live_listener =
            TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, live_port)).unwrap();
        let next_port = req(
            "next-port",
            vec![HostResourceRequirement {
                key: "port-range".into(),
                resource: HostResourceKind::TcpPort {
                    start: dead_port,
                    end: live_port,
                },
            }],
        );
        let tcp_explanation = c.explain(&next_port).unwrap();
        assert!(tcp_explanation.wait.waitable);
        assert_eq!(tcp_explanation.wait.reason, "resource_contention");
        let mut tcp_callbacks = 0;
        let tcp_error = c
            .acquire_with_wait(&next_port, std::time::Duration::from_millis(250), |_| {
                tcp_callbacks += 1
            })
            .unwrap_err();
        assert_eq!(conflict_code(tcp_error), "resource_contention");
        assert!(
            tcp_callbacks > 0,
            "the live listener may release another port in the requested range"
        );
        let retained_dead_port = c
            .list(false)
            .unwrap()
            .into_iter()
            .find(|lease| lease.lease_id == dead_port_lease.lease.lease_id)
            .unwrap();
        assert_eq!(
            retained_dead_port.state,
            HostLeaseState::Active,
            "a retryable TCP contention refusal rolls back the opportunistic quarantine sweep"
        );
        assert_eq!(
            retained_dead_port.allocations, dead_port_lease.lease.allocations,
            "the refused request must not quietly release the dead holder's named TCP allocation"
        );
        drop(live_listener);
    }

    fn adjacent_bindable_ports() -> (u16, u16) {
        for _ in 0..8 {
            let anchor = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
            let pivot = anchor.local_addr().unwrap().port();
            drop(anchor);
            let first = pivot.saturating_sub(64).max(1_024);
            let last = pivot.saturating_add(64).min(u16::MAX - 1);
            for port in first..=last {
                let Ok(first_listener) =
                    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
                else {
                    continue;
                };
                let Ok(second_listener) =
                    TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port + 1))
                else {
                    continue;
                };
                drop(second_listener);
                drop(first_listener);
                return (port, port + 1);
            }
        }
        panic!("could not find a free adjacent TCP port pair for the test");
    }

    /// #1235: several ports over one range in one bundle are distinct, in
    /// the plan and the grant; a range too narrow for all of them is refused
    /// whole, never granted with a duplicate.
    #[test]
    fn ports_in_one_bundle_are_distinct_or_refused_together() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let (first, second) = adjacent_bindable_ports();
        let ports = |id: &str, keys: &[&str]| {
            req(
                id,
                keys.iter()
                    .map(|key| HostResourceRequirement {
                        key: (*key).into(),
                        resource: HostResourceKind::TcpPort {
                            start: first,
                            end: second,
                        },
                    })
                    .collect(),
            )
        };
        let values = |allocations: &[HostResourceAllocation]| {
            let mut values: Vec<String> = allocations.iter().map(|a| a.value.clone()).collect();
            values.sort();
            values
        };
        let expected = vec![first.to_string(), second.to_string()];

        let pair = ports("pair", &["port_api", "port_db"]);
        let plan = c.plan(&pair).unwrap();
        assert!(plan.available, "{plan:?}");
        assert_eq!(values(&plan.proposed), expected);
        let grant = c.acquire(&pair).unwrap();
        assert_eq!(values(&grant.lease.allocations), expected);
        c.release(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        )
        .unwrap();

        let three = ports("three", &["port_api", "port_db", "port_web"]);
        let plan = c.plan(&three).unwrap();
        assert!(!plan.available, "{plan:?}");
        let error = c.acquire(&three).unwrap_err();
        assert_eq!(conflict_code(error), "resource_contention");
        assert!(c.list(false).unwrap().is_empty(), "nothing was granted");
    }

    #[test]
    fn capacity_policy_mismatch_keeps_precedence_and_its_original_owner() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let mut dead_key = req("dead-key", vec![exclusive("shared")]);
        dead_key.holder_pid = Some(reaped_pid());
        c.acquire(&dead_key).unwrap();
        let live_capacity =
            capacity_request("live-capacity", "workers", 1, 4, Some(std::process::id()));
        let capacity_lease = c.acquire(&live_capacity).unwrap();

        let request = req(
            "bundle",
            vec![
                exclusive("shared"),
                HostResourceRequirement {
                    key: "workers".into(),
                    resource: HostResourceKind::Capacity {
                        pool: "workers".into(),
                        units: 1,
                        limit: 5,
                    },
                },
            ],
        );
        let error = c.acquire(&request).unwrap_err();
        let HostResourceError::Conflict {
            code, conflicts, ..
        } = error
        else {
            panic!("both the dead key and mismatched pool policy must block the bundle");
        };
        assert_eq!(code, "capacity_policy_mismatch");
        assert!(conflicts.iter().any(|conflict| {
            conflict.code == ORPHANED_HOLDER_CONFLICT_CODE && conflict.kind == "exclusive_key"
        }));
        let policy_conflict = conflicts
            .iter()
            .find(|conflict| conflict.code == "capacity_policy_mismatch")
            .unwrap();
        assert_eq!(
            policy_conflict.owning_lease.as_deref(),
            Some(capacity_lease.lease.lease_id.as_str())
        );
    }

    #[test]
    fn unknown_holder_pid_keeps_ordinary_contention_waitable() {
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();
        let mut holder = req("unknown-holder", vec![exclusive("shared")]);
        holder.holder_pid = None;
        c.acquire(&holder).unwrap();

        let mut callbacks = 0;
        let error = c
            .acquire_with_wait(
                &req("next", vec![exclusive("shared")]),
                std::time::Duration::from_millis(250),
                |_| callbacks += 1,
            )
            .unwrap_err();
        assert_eq!(conflict_code(error), "resource_contention");
        assert!(
            callbacks > 0,
            "an unknown holder must retain ordinary retry behavior"
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

    #[test]
    fn resource_lease_candidates_include_expiry_boundary_and_skip_active_or_released() {
        const NOW: i64 = 100;
        let t = tempfile::tempdir().unwrap();
        let mut c = HostResourceCoordinator::open(&t.path().join("h.db")).unwrap();

        let boundary = c
            .acquire(&req("boundary", vec![exclusive("boundary")]))
            .unwrap();
        let future = c
            .acquire(&req("future", vec![exclusive("future")]))
            .unwrap();
        let quarantined = c
            .acquire(&req("quarantined", vec![exclusive("quarantined")]))
            .unwrap();
        c.quarantine(
            &quarantined.lease.lease_id,
            quarantined.lease.generation,
            &quarantined.ownership_token,
        )
        .unwrap();
        let released = c
            .acquire(&req("released", vec![exclusive("released")]))
            .unwrap();
        c.release(
            &released.lease.lease_id,
            released.lease.generation,
            &released.ownership_token,
        )
        .unwrap();

        c.conn
            .execute(
                "UPDATE resource_leases SET expires_at=?1 WHERE lease_id=?2",
                rusqlite::params![NOW, boundary.lease.lease_id.as_str()],
            )
            .unwrap();
        c.conn
            .execute(
                "UPDATE resource_leases SET expires_at=?1 WHERE lease_id=?2",
                rusqlite::params![NOW + 1, future.lease.lease_id.as_str()],
            )
            .unwrap();

        let leases = c.quarantined_leases_at(NOW).unwrap();
        let ids = leases
            .iter()
            .map(|lease| lease.lease_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                boundary.lease.lease_id.clone(),
                quarantined.lease.lease_id.clone()
            ]
        );
        assert!(
            leases
                .iter()
                .all(|lease| lease.state == HostLeaseState::Quarantined)
        );
        assert!(leases.iter().all(|lease| lease.allocations.len() == 1));
    }
}
