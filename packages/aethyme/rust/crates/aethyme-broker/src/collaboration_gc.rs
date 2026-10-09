//! Reference-safe reclamation of retained collaboration data (#659, plan
//! §6.3, §6.16, §10.1, §10.9; D20, D25, D29; T45, T46).
//!
//! - **Roots.** Live `contribution` retention roots (unreleased and not past
//!   their `until`), live reader leases and live object pins. Everything they
//!   reach is kept: contribution → lineage record → base and result snapshots
//!   → manifests and snapshot records → blobs, plus the receipt of every
//!   operation whose root is live. A root that cannot be resolved blocks all
//!   reclamation instead of shrinking what is kept.
//! - **Grace.** An unrooted object, snapshot or contribution is kept until its
//!   newest object has been unwritten for the grace period; the archive
//!   refreshes an object's time when a capture reuses it. Unrecognised files
//!   are never removed, only reported.
//! - **Plan and apply.** A plan is recorded under a digest. `apply` takes the
//!   archive exclusively (captures, recovery, reconstruction, leases and pins
//!   hold it shared), recomputes eligibility, and removes only what the plan
//!   named and is still eligible; anything else is skipped and reported.
//! - **Generations.** One apply is one generation: a transaction records what
//!   it will remove and marks reclaimed snapshots and contributions; files then
//!   move into `spool/gc/<generation>/` and are deleted. An interrupted
//!   generation is resumed by [`resume`], which revalidates every file first,
//!   so nothing a later capture came to rely on is removed.
//! - **Authority.** Only this module reclaims collaboration data. Legacy
//!   broker cleanup cannot reach the root (#656) and is not wired here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use aethyme_contracts::experimental_v0::SourceSnapshotId;
use rusqlite::{OptionalExtension, TransactionBehavior};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::collaboration_archive::{
    ObjectDigest, object_path, objects_dir, parse_manifest, read_object, spool_dir,
};
use crate::collaboration_state::{CollaborationStateError, CollaborationStore, sync_directory};
use crate::file_lock::open_lock_file;

/// How long an unrooted object is kept after it was last written.
pub const DEFAULT_GRACE_MS: i64 = 24 * 60 * 60 * 1000;
/// How long a recorded plan may be applied.
pub const PLAN_TTL_MS: i64 = 24 * 60 * 60 * 1000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

// ------------------------------------------------------------ archive lock

/// The lock every archive user shares and reclamation takes exclusively.
pub(crate) fn lock_path(store: &CollaborationStore) -> PathBuf {
    store.project_dir().join("spool/gc.lock")
}

fn open_archive_lock(store: &CollaborationStore) -> std::io::Result<std::fs::File> {
    let path = lock_path(store);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        crate::host_state::protect_host_state_path(parent, true)?;
    }
    open_lock_file(&path)
}

/// The archive held shared: reclamation cannot apply until it drops.
#[derive(Debug)]
pub struct ArchiveUse {
    file: std::fs::File,
}

impl Drop for ArchiveUse {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Hold the archive shared, waiting while a reclamation applies.
pub fn archive_use(store: &CollaborationStore) -> std::io::Result<ArchiveUse> {
    let file = open_archive_lock(store)?;
    file.lock_shared()?;
    Ok(ArchiveUse { file })
}

struct ArchiveExclusive {
    file: std::fs::File,
}

impl Drop for ArchiveExclusive {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn try_exclusive(store: &CollaborationStore) -> Result<Option<ArchiveExclusive>, GcError> {
    let file = open_archive_lock(store).map_err(|source| GcError::Io {
        path: lock_path(store),
        source,
    })?;
    match file.try_lock() {
        Ok(()) => Ok(Some(ArchiveExclusive { file })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(source)) => Err(GcError::Io {
            path: lock_path(store),
            source,
        }),
    }
}

// ------------------------------------------------------------------ types

/// Why an object is kept, or what it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionClass {
    /// Blobs and snapshot manifests.
    Source,
    /// Snapshot and lineage records.
    Record,
    /// Capture receipts.
    Receipt,
    /// Pinned by an analysis view (placeholder until L3/AQ producers pin).
    AnalysisView,
    /// Pinned as cited evidence (placeholder until L6 cites).
    CitedEvidence,
    /// Named by nothing: a failed capture's leftovers or a temporary file.
    Orphan,
}

/// What one plan item removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Object,
    /// An abandoned archive temporary under `spool/archive`.
    SpoolTemporary,
    /// The lock file of an operation that has ended.
    OperationLock,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct GcItem {
    pub kind: ItemKind,
    /// Relative to the project directory.
    pub relpath: String,
    pub bytes: u64,
    pub class: RetentionClass,
}

/// An index entry that is marked reclaimed when its objects go.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum GcEntity {
    Snapshot(String),
    Contribution(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GcBlocker {
    /// `capture_in_progress`, `unrecovered_capture`, `live_capture_intent`,
    /// `dangling_root` or `interrupted_gc`.
    pub kind: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ClassTotal {
    pub objects: u64,
    pub bytes: u64,
}

/// What a reclamation would do, recorded under its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GcPlan {
    pub digest: String,
    pub created_ms: i64,
    pub grace_ms: i64,
    /// Kept because a live root reaches it, by class.
    pub retained: BTreeMap<RetentionClass, ClassTotal>,
    /// Unrooted but inside the grace period, or held by a young entry.
    pub protected_bytes: u64,
    /// Files under `objects/` or `spool/archive/` that are not archive
    /// objects or temporaries. Never removed.
    pub unknown: Vec<String>,
    pub reclaimable: Vec<GcItem>,
    pub reclaimable_bytes: u64,
    pub entities: Vec<GcEntity>,
    /// Any blocker empties `reclaimable`.
    pub blockers: Vec<GcBlocker>,
    pub next_action: String,
}

/// What one apply did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GcReport {
    pub generation: Option<i64>,
    pub reclaimed: Vec<GcItem>,
    pub reclaimed_bytes: u64,
    pub entities: Vec<GcEntity>,
    /// Planned items no longer eligible, with the reason; left in place.
    pub skipped: Vec<(GcItem, &'static str)>,
}

/// What [`resume`] did with an interrupted generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resumed {
    pub generation: i64,
    /// Files removed as planned.
    pub removed: usize,
    /// Files put back because something came to rely on them.
    pub restored: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcOptions {
    pub grace_ms: i64,
    /// The time to judge expiry and grace by; `None` is now.
    pub now_ms: Option<i64>,
}

impl Default for GcOptions {
    fn default() -> Self {
        Self {
            grace_ms: DEFAULT_GRACE_MS,
            now_ms: None,
        }
    }
}

impl GcOptions {
    fn now(&self) -> i64 {
        self.now_ms.unwrap_or_else(now_ms)
    }
}

/// What a reader lease keeps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseTarget {
    Snapshot(SourceSnapshotId),
    /// A contribution's lineage record ID.
    Contribution(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinClass {
    AnalysisView,
    CitedEvidence,
}

#[derive(Debug, thiserror::Error)]
pub enum GcError {
    #[error("the archive is in use by a capture, recovery or reader; apply again when they finish")]
    ArchiveInUse,
    #[error("reclamation is blocked: {}", .0.iter().map(|b| format!("{} ({})", b.kind, b.detail)).collect::<Vec<_>>().join("; "))]
    Blocked(Vec<GcBlocker>),
    #[error("no recorded plan {digest}; run plan and confirm the digest it prints")]
    UnknownPlan { digest: String },
    #[error("plan {digest} is older than a day; run plan again")]
    StalePlan { digest: String },
    #[error("{what} is not retained, so it cannot be leased or pinned")]
    NotRetained { what: String },
    #[error("holder must be 1-200 bytes")]
    InvalidHolder,
    #[cfg(test)]
    #[error("injected fault after {0} moves")]
    Injected(usize),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("collaboration state: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    State(#[from] CollaborationStateError),
}

impl GcError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ArchiveInUse => "archive_in_use",
            Self::Blocked(_) => "blocked",
            Self::UnknownPlan { .. } => "unknown_plan",
            Self::StalePlan { .. } => "stale_plan",
            Self::NotRetained { .. } => "not_retained",
            Self::InvalidHolder => "invalid_holder",
            #[cfg(test)]
            Self::Injected(_) => "injected",
            Self::Io { .. } => "io",
            Self::Sqlite(_) => "sqlite",
            Self::State(_) => "state",
        }
    }
}

fn io(path: &Path, source: std::io::Error) -> GcError {
    GcError::Io {
        path: path.to_path_buf(),
        source,
    }
}

// ------------------------------------------------------- leases and pins

fn check_holder(holder: &str) -> Result<(), GcError> {
    if holder.is_empty() || holder.len() > 200 {
        return Err(GcError::InvalidHolder);
    }
    Ok(())
}

fn snapshot_is_retained(store: &CollaborationStore, id: &str) -> Result<bool, GcError> {
    Ok(store
        .read_connection()
        .query_row(
            "SELECT 1 FROM retained_snapshots WHERE snapshot_id = ?1
               AND snapshot_id NOT IN (SELECT snapshot_id FROM reclaimed_snapshots)",
            [id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Keep `target` until `until_ms`, or until released. Refused when the target
/// is not retained now: a lease never resurrects reclaimed source. Taken
/// while holding the archive, so no reclamation interleaves.
pub fn acquire_lease(
    store: &mut CollaborationStore,
    target: &LeaseTarget,
    holder: &str,
    until_ms: i64,
) -> Result<i64, GcError> {
    check_holder(holder)?;
    let _use = archive_use(store).map_err(|source| io(&lock_path(store), source))?;
    let (kind, id, retained) = match target {
        LeaseTarget::Snapshot(id) => (
            "snapshot",
            id.as_str().to_string(),
            snapshot_is_retained(store, id.as_str())?,
        ),
        LeaseTarget::Contribution(lineage) => {
            let retained = store
                .read_connection()
                .query_row(
                    "SELECT base_snapshot, result_snapshot FROM retained_contributions
                     WHERE lineage_record_id = ?1 AND lineage_record_id NOT IN
                         (SELECT lineage_record_id FROM reclaimed_contributions)",
                    [lineage],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            let retained = match retained {
                Some((base, result)) => {
                    snapshot_is_retained(store, &base)? && snapshot_is_retained(store, &result)?
                }
                None => false,
            };
            ("contribution", lineage.clone(), retained)
        }
    };
    if !retained {
        return Err(GcError::NotRetained { what: id });
    }
    store.connection().execute(
        "INSERT INTO reader_leases (target_kind, target, holder, until_ms, created_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![kind, id, holder, until_ms, now_ms()],
    )?;
    Ok(store.connection().last_insert_rowid())
}

pub fn release_lease(store: &mut CollaborationStore, lease_id: i64) -> Result<bool, GcError> {
    Ok(store.connection().execute(
        "UPDATE reader_leases SET released_ms = ?2 WHERE lease_id = ?1 AND released_ms IS NULL",
        rusqlite::params![lease_id, now_ms()],
    )? == 1)
}

/// Keep one archive object for an analysis view or as cited evidence.
pub fn pin_object(
    store: &mut CollaborationStore,
    class: PinClass,
    digest: &ObjectDigest,
    holder: &str,
    until_ms: Option<i64>,
) -> Result<i64, GcError> {
    check_holder(holder)?;
    let _use = archive_use(store).map_err(|source| io(&lock_path(store), source))?;
    if read_object(store, digest).is_err() {
        return Err(GcError::NotRetained {
            what: format!("object {}", digest.hex()),
        });
    }
    let class = match class {
        PinClass::AnalysisView => "analysis_view",
        PinClass::CitedEvidence => "cited_evidence",
    };
    store.connection().execute(
        "INSERT INTO object_pins (class, object_sha256, holder, until_ms, created_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![class, digest.hex(), holder, until_ms, now_ms()],
    )?;
    Ok(store.connection().last_insert_rowid())
}

pub fn release_pin(store: &mut CollaborationStore, pin_id: i64) -> Result<bool, GcError> {
    Ok(store.connection().execute(
        "UPDATE object_pins SET released_ms = ?2 WHERE pin_id = ?1 AND released_ms IS NULL",
        rusqlite::params![pin_id, now_ms()],
    )? == 1)
}

/// Release an operation's contribution root. Its source stays while any
/// other root, lease or pin reaches it.
pub fn release_retention(
    store: &mut CollaborationStore,
    operation_id: &str,
) -> Result<bool, GcError> {
    Ok(store.connection().execute(
        "UPDATE retention_roots SET released_ms = ?2
         WHERE operation_id = ?1 AND kind = 'contribution' AND released_ms IS NULL",
        rusqlite::params![operation_id, now_ms()],
    )? > 0)
}

// ----------------------------------------------------------------- survey

fn parse_hex_digest(text: &str) -> Option<ObjectDigest> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut bytes = [0; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(ObjectDigest::from_bytes(bytes))
}

fn object_relpath(digest: &ObjectDigest) -> String {
    let hex = digest.hex();
    format!("objects/sha256/{}/{}", &hex[..2], &hex[2..])
}

fn modified_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let elapsed = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(elapsed.as_millis()).ok()
}

/// The objects one index entry names, with their classes.
struct Closure<'a> {
    store: &'a CollaborationStore,
    /// Object → class; the first class recorded wins.
    objects: BTreeMap<ObjectDigest, RetentionClass>,
    snapshots: BTreeSet<String>,
    contributions: BTreeSet<String>,
    /// Why a root could not be followed, when strict.
    problems: Vec<String>,
}

impl<'a> Closure<'a> {
    fn new(store: &'a CollaborationStore) -> Self {
        Self {
            store,
            objects: BTreeMap::new(),
            snapshots: BTreeSet::new(),
            contributions: BTreeSet::new(),
            problems: Vec::new(),
        }
    }

    fn object(&mut self, digest: ObjectDigest, class: RetentionClass) {
        if !object_path(self.store, &digest).is_file() {
            self.problems
                .push(format!("object {} is missing", digest.hex()));
        }
        self.objects.entry(digest).or_insert(class);
    }

    fn snapshot(&mut self, id: &str) -> Result<(), GcError> {
        if !self.snapshots.insert(id.to_string()) {
            return Ok(());
        }
        let record: Option<String> = self
            .store
            .read_connection()
            .query_row(
                "SELECT record_sha256 FROM retained_snapshots WHERE snapshot_id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(record) = record.as_deref().and_then(parse_hex_digest) else {
            self.problems.push(format!("snapshot {id} is not indexed"));
            return Ok(());
        };
        self.object(record, RetentionClass::Record);
        let Ok(snapshot_id) = SourceSnapshotId::parse(id) else {
            self.problems
                .push(format!("snapshot {id} has a malformed id"));
            return Ok(());
        };
        let manifest_digest = ObjectDigest::of_snapshot(&snapshot_id);
        self.object(manifest_digest, RetentionClass::Source);
        let Some(snapshot) = read_object(self.store, &manifest_digest)
            .ok()
            .and_then(|bytes| parse_manifest(&bytes))
        else {
            self.problems
                .push(format!("the manifest of snapshot {id} is unreadable"));
            return Ok(());
        };
        for entry in snapshot.entries() {
            self.object(
                ObjectDigest::from_bytes(*entry.content_sha256()),
                RetentionClass::Source,
            );
        }
        Ok(())
    }

    fn contribution(&mut self, lineage: &str) -> Result<(), GcError> {
        if !self.contributions.insert(lineage.to_string()) {
            return Ok(());
        }
        let row: Option<(String, String, String)> = self
            .store
            .read_connection()
            .query_row(
                "SELECT record_sha256, base_snapshot, result_snapshot
                 FROM retained_contributions WHERE lineage_record_id = ?1",
                [lineage],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((record, base, result)) = row else {
            self.problems
                .push(format!("contribution {lineage} is not indexed"));
            return Ok(());
        };
        match parse_hex_digest(&record) {
            Some(digest) => self.object(digest, RetentionClass::Record),
            None => self
                .problems
                .push(format!("contribution {lineage} has a malformed record")),
        }
        self.snapshot(&base)?;
        self.snapshot(&result)
    }
}

/// Everything a plan is computed from.
struct Survey {
    blockers: Vec<GcBlocker>,
    retained: BTreeMap<RetentionClass, ClassTotal>,
    protected_bytes: u64,
    unknown: Vec<String>,
    candidates: BTreeMap<String, GcItem>,
    entities: BTreeSet<GcEntity>,
}

/// `resuming` is a generation the caller is finishing, which is therefore
/// not a blocker.
fn survey(
    store: &CollaborationStore,
    options: &GcOptions,
    resuming: Option<i64>,
) -> Result<Survey, GcError> {
    let now = options.now();
    let connection = store.read_connection();
    let mut blockers = Vec::new();

    // Captures that are not over.
    let active: Vec<String> = connection
        .prepare(
            "SELECT operation_id FROM capture_operations
             WHERE state IN ('intent', 'copying', 'sealed') ORDER BY operation_id",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for operation in &active {
        let path = crate::collaboration_capture::locks_dir(store).join(format!("{operation}.lock"));
        let live = open_lock_file(&path)
            .is_ok_and(|file| matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        blockers.push(if live {
            GcBlocker {
                kind: "capture_in_progress",
                detail: format!("capture {operation} is running"),
            }
        } else {
            GcBlocker {
                kind: "unrecovered_capture",
                detail: format!(
                    "capture {operation} stopped without an outcome; run \
                     collaboration_capture::recover first"
                ),
            }
        });
    }
    let stray_intents: Vec<String> = connection
        .prepare(
            "SELECT r.operation_id FROM retention_roots r JOIN capture_operations o
                 USING (operation_id)
             WHERE r.kind = 'capture_intent' AND r.released_ms IS NULL
               AND o.state NOT IN ('intent', 'copying', 'sealed')",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for operation in stray_intents {
        blockers.push(GcBlocker {
            kind: "live_capture_intent",
            detail: format!(
                "capture {operation} has ended but still holds an intent root, which protects \
                 objects nothing indexes"
            ),
        });
    }
    let interrupted: Vec<i64> = connection
        .prepare(
            "SELECT generation FROM gc_generations
             WHERE state != 'done' AND generation IS NOT ?1",
        )?
        .query_map([resuming], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for generation in interrupted {
        blockers.push(GcBlocker {
            kind: "interrupted_gc",
            detail: format!(
                "reclamation generation {generation} was interrupted; run \
                 collaboration_gc::resume first"
            ),
        });
    }

    // Live roots and what they reach.
    let mut live = Closure::new(store);
    let roots: Vec<(String, Option<String>)> = connection
        .prepare(
            "SELECT operation_id, lineage_record_id FROM retention_roots
             WHERE kind = 'contribution' AND released_ms IS NULL
               AND (until_ms IS NULL OR until_ms > ?1)",
        )?
        .query_map([now], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (operation, lineage) in &roots {
        match lineage {
            Some(lineage) => live.contribution(lineage)?,
            None => live.problems.push(format!(
                "the root of capture {operation} names no contribution"
            )),
        }
        let receipt: Option<String> = connection
            .query_row(
                "SELECT receipt_sha256 FROM capture_receipts WHERE operation_id = ?1",
                [operation],
                |row| row.get(0),
            )
            .optional()?;
        match receipt.as_deref().and_then(parse_hex_digest) {
            Some(digest) => live.object(digest, RetentionClass::Receipt),
            None => live
                .problems
                .push(format!("the receipt of capture {operation} is not indexed")),
        }
    }
    let leases: Vec<(String, String)> = connection
        .prepare(
            "SELECT target_kind, target FROM reader_leases
             WHERE released_ms IS NULL AND until_ms > ?1",
        )?
        .query_map([now], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (kind, target) in &leases {
        if kind == "snapshot" {
            live.snapshot(target)?;
        } else {
            live.contribution(target)?;
        }
    }
    let pins: Vec<(String, String)> = connection
        .prepare(
            "SELECT class, object_sha256 FROM object_pins
             WHERE released_ms IS NULL AND (until_ms IS NULL OR until_ms > ?1)",
        )?
        .query_map([now], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (class, digest) in &pins {
        let class = if class == "analysis_view" {
            RetentionClass::AnalysisView
        } else {
            RetentionClass::CitedEvidence
        };
        match parse_hex_digest(digest) {
            Some(digest) => live.object(digest, class),
            None => live.problems.push(format!("pin of {digest} is malformed")),
        }
    }
    for problem in std::mem::take(&mut live.problems) {
        blockers.push(GcBlocker {
            kind: "dangling_root",
            detail: problem,
        });
    }

    // Unrooted index entries: young ones keep what they name; old ones are
    // reclaimed as a unit.
    let young = |digests: &[ObjectDigest]| {
        digests.iter().any(|digest| {
            modified_ms(&object_path(store, digest)).is_some_and(|ms| ms > now - options.grace_ms)
        })
    };
    let mut kept = Closure::new(store);
    let mut entities = BTreeSet::new();
    let contributions: Vec<(String, String)> = connection
        .prepare(
            "SELECT lineage_record_id, record_sha256 FROM retained_contributions
             WHERE lineage_record_id NOT IN (SELECT lineage_record_id FROM reclaimed_contributions)",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (lineage, record) in &contributions {
        if live.contributions.contains(lineage) {
            continue;
        }
        let record: Vec<ObjectDigest> = parse_hex_digest(record).into_iter().collect();
        if young(&record) {
            kept.contribution(lineage)?;
        } else {
            entities.insert(GcEntity::Contribution(lineage.clone()));
        }
    }
    let snapshots: Vec<(String, String)> = connection
        .prepare(
            "SELECT snapshot_id, record_sha256 FROM retained_snapshots
             WHERE snapshot_id NOT IN (SELECT snapshot_id FROM reclaimed_snapshots)",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (id, record) in &snapshots {
        if live.snapshots.contains(id) || kept.snapshots.contains(id) {
            continue;
        }
        let mut digests: Vec<ObjectDigest> = parse_hex_digest(record).into_iter().collect();
        if let Ok(snapshot_id) = SourceSnapshotId::parse(id) {
            digests.push(ObjectDigest::of_snapshot(&snapshot_id));
        }
        if young(&digests) {
            kept.snapshot(id)?;
        } else {
            entities.insert(GcEntity::Snapshot(id.clone()));
        }
    }
    // Classes for what old entries and released receipts name.
    let mut known: BTreeMap<ObjectDigest, RetentionClass> = BTreeMap::new();
    {
        let mut old = Closure::new(store);
        for entity in &entities {
            match entity {
                GcEntity::Snapshot(id) => old.snapshot(id)?,
                GcEntity::Contribution(lineage) => old.contribution(lineage)?,
            }
        }
        known.extend(old.objects);
        let receipts: Vec<String> = connection
            .prepare("SELECT receipt_sha256 FROM capture_receipts")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        for receipt in receipts {
            if let Some(digest) = parse_hex_digest(&receipt) {
                known.entry(digest).or_insert(RetentionClass::Receipt);
            }
        }
    }

    // Every file in the archive.
    let mut retained: BTreeMap<RetentionClass, ClassTotal> = BTreeMap::new();
    let mut protected_bytes = 0;
    let mut unknown = Vec::new();
    let mut candidates = BTreeMap::new();
    let objects = objects_dir(store);
    let mut fan_outs: Vec<PathBuf> = match std::fs::read_dir(&objects) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<_, _>>()
            .map_err(|source| io(&objects, source))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(source) => return Err(io(&objects, source)),
    };
    fan_outs.sort();
    for fan_out in fan_outs {
        let prefix = fan_out
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let relative = |path: &Path| {
            path.strip_prefix(store.project_dir())
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned()
        };
        if !fan_out.is_dir() || parse_hex_digest(&format!("{prefix}{}", "0".repeat(62))).is_none() {
            unknown.push(relative(&fan_out));
            continue;
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&fan_out)
            .map_err(|source| io(&fan_out, source))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<_, _>>()
            .map_err(|source| io(&fan_out, source))?;
        files.sort();
        for file in files {
            let name = file
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let metadata = std::fs::symlink_metadata(&file).map_err(|source| io(&file, source))?;
            let Some(digest) =
                parse_hex_digest(&format!("{prefix}{name}")).filter(|_| metadata.is_file())
            else {
                unknown.push(relative(&file));
                continue;
            };
            let bytes = metadata.len();
            if let Some(class) = live.objects.get(&digest) {
                let total = retained.entry(*class).or_default();
                total.objects += 1;
                total.bytes += bytes;
                continue;
            }
            let recent = modified_ms(&file).is_none_or(|ms| ms > now - options.grace_ms);
            if kept.objects.contains_key(&digest) || recent {
                protected_bytes += bytes;
                continue;
            }
            let relpath = object_relpath(&digest);
            candidates.insert(
                relpath.clone(),
                GcItem {
                    kind: ItemKind::Object,
                    relpath,
                    bytes,
                    class: known
                        .get(&digest)
                        .copied()
                        .unwrap_or(RetentionClass::Orphan),
                },
            );
        }
    }

    // Abandoned archive temporaries and ended operations' lock files.
    let spool = spool_dir(store);
    if let Ok(entries) = std::fs::read_dir(&spool) {
        for entry in entries {
            let path = entry.map_err(|source| io(&spool, source))?.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            let relpath = format!("spool/archive/{name}");
            if !name.starts_with(".object-") || !path.is_file() {
                unknown.push(relpath);
                continue;
            }
            if modified_ms(&path).is_none_or(|ms| ms > now - options.grace_ms) {
                continue;
            }
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            candidates.insert(
                relpath.clone(),
                GcItem {
                    kind: ItemKind::SpoolTemporary,
                    relpath,
                    bytes,
                    class: RetentionClass::Orphan,
                },
            );
        }
    }
    let locks = crate::collaboration_capture::locks_dir(store);
    if let Ok(entries) = std::fs::read_dir(&locks) {
        for entry in entries {
            let path = entry.map_err(|source| io(&locks, source))?.path();
            let Some(operation) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".lock"))
            else {
                continue;
            };
            let state: Option<String> = connection
                .query_row(
                    "SELECT state FROM capture_operations WHERE operation_id = ?1",
                    [operation],
                    |row| row.get(0),
                )
                .optional()?;
            let ended = matches!(
                state.as_deref(),
                Some("committed" | "acknowledged" | "refused" | "aborted")
            );
            if !ended || modified_ms(&path).is_none_or(|ms| ms > now - options.grace_ms) {
                continue;
            }
            let relpath = format!("spool/capture/{operation}.lock");
            candidates.insert(
                relpath.clone(),
                GcItem {
                    kind: ItemKind::OperationLock,
                    relpath,
                    bytes: 0,
                    class: RetentionClass::Orphan,
                },
            );
        }
    }

    if !blockers.is_empty() {
        candidates.clear();
        entities.clear();
    }
    unknown.sort();
    Ok(Survey {
        blockers,
        retained,
        protected_bytes,
        unknown,
        candidates,
        entities,
    })
}

fn plan_digest(items: &[GcItem], entities: &[GcEntity]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aethyme collaboration gc plan v0\0");
    for item in items {
        hasher.update(format!("{:?}\0{}\0{}\0", item.kind, item.relpath, item.bytes).as_bytes());
    }
    for entity in entities {
        hasher.update(format!("{entity:?}\0").as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredPlan {
    items: Vec<(String, String, u64)>,
    entities: Vec<(String, String)>,
}

fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Object => "object",
        ItemKind::SpoolTemporary => "spool_temporary",
        ItemKind::OperationLock => "operation_lock",
    }
}

/// Survey the archive and record a plan. Removes nothing.
pub fn plan(store: &mut CollaborationStore, options: &GcOptions) -> Result<GcPlan, GcError> {
    let survey = survey(store, options, None)?;
    let reclaimable: Vec<GcItem> = survey.candidates.into_values().collect();
    let entities: Vec<GcEntity> = survey.entities.into_iter().collect();
    let digest = plan_digest(&reclaimable, &entities);
    let created_ms = options.now();
    let reclaimable_bytes = reclaimable.iter().map(|item| item.bytes).sum();
    let next_action = if let Some(blocker) = survey.blockers.first() {
        match blocker.kind {
            "capture_in_progress" => "wait for running captures to finish, then plan again".into(),
            "unrecovered_capture" => {
                "run collaboration_capture::recover, then plan again".to_string()
            }
            "interrupted_gc" => "run collaboration_gc::resume, then plan again".into(),
            _ => format!(
                "repair the store before reclaiming anything: {}",
                blocker.detail
            ),
        }
    } else if reclaimable.is_empty() && entities.is_empty() {
        "nothing to reclaim".into()
    } else {
        format!("collaboration_gc::apply with digest {digest}")
    };
    if survey.blockers.is_empty() && !(reclaimable.is_empty() && entities.is_empty()) {
        let stored = StoredPlan {
            items: reclaimable
                .iter()
                .map(|item| {
                    (
                        kind_name(item.kind).into(),
                        item.relpath.clone(),
                        item.bytes,
                    )
                })
                .collect(),
            entities: entities
                .iter()
                .map(|entity| match entity {
                    GcEntity::Snapshot(id) => ("snapshot".into(), id.clone()),
                    GcEntity::Contribution(id) => ("contribution".into(), id.clone()),
                })
                .collect(),
        };
        store.connection().execute(
            "INSERT OR REPLACE INTO gc_plans (digest, body, created_ms) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                digest,
                serde_json::to_string(&stored).expect("plans serialize"),
                created_ms
            ],
        )?;
    }
    Ok(GcPlan {
        digest,
        created_ms,
        grace_ms: options.grace_ms,
        retained: survey.retained,
        protected_bytes: survey.protected_bytes,
        unknown: survey.unknown,
        reclaimable,
        reclaimable_bytes,
        entities,
        blockers: survey.blockers,
        next_action,
    })
}

// ------------------------------------------------------------------ apply

/// Test seams. Production passes the default.
#[derive(Default)]
pub(crate) struct GcHooks {
    /// Fail with an injected ENOSPC-like error after this many moves.
    #[cfg(test)]
    pub fail_after_moves: Option<usize>,
    /// Stop forever after this many moves (the process is then killed).
    #[cfg(test)]
    pub stall_after_moves: Option<usize>,
    /// Called after this many moves, with the archive still held.
    #[cfg(test)]
    pub after_moves: Option<(usize, Box<dyn Fn() + Send + Sync>)>,
}

impl GcHooks {
    #[cfg(test)]
    fn moved(&self, count: usize) -> Result<(), GcError> {
        if let Some((at, callback)) = &self.after_moves
            && *at == count
        {
            callback();
        }
        if self.stall_after_moves == Some(count) {
            println!("stalled");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
        if self.fail_after_moves == Some(count) {
            return Err(GcError::Injected(count));
        }
        Ok(())
    }

    #[cfg(not(test))]
    #[inline]
    fn moved(&self, _count: usize) -> Result<(), GcError> {
        Ok(())
    }
}

fn trash_dir(store: &CollaborationStore, generation: i64) -> PathBuf {
    store.project_dir().join(format!("spool/gc/{generation}"))
}

fn trash_path(store: &CollaborationStore, generation: i64, relpath: &str) -> PathBuf {
    trash_dir(store, generation).join(relpath.replace('/', "%"))
}

/// Move `relpath` into the generation's trash. Already moved is success.
fn move_to_trash(
    store: &CollaborationStore,
    generation: i64,
    relpath: &str,
    touched: &mut BTreeSet<PathBuf>,
) -> Result<(), GcError> {
    let source = store.project_dir().join(relpath);
    let target = trash_path(store, generation, relpath);
    match std::fs::rename(&source, &target) {
        Ok(()) => {
            if let Some(parent) = source.parent() {
                touched.insert(parent.to_path_buf());
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source_error) => Err(io(&source, source_error)),
    }
}

fn finish_generation(store: &mut CollaborationStore, generation: i64) -> Result<(), GcError> {
    store.connection().execute(
        "UPDATE gc_generations SET state = 'trashed' WHERE generation = ?1",
        [generation],
    )?;
    let dir = trash_dir(store, generation);
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(io(&dir, source)),
    }
    if let Some(parent) = dir.parent() {
        sync_directory(parent)?;
    }
    store.connection().execute(
        "UPDATE gc_generations SET state = 'done', finished_ms = ?2 WHERE generation = ?1",
        rusqlite::params![generation, now_ms()],
    )?;
    Ok(())
}

/// Remove what plan `digest` named and is still eligible.
pub fn apply(
    store: &mut CollaborationStore,
    digest: &str,
    options: &GcOptions,
) -> Result<GcReport, GcError> {
    apply_with(store, digest, options, &GcHooks::default())
}

pub(crate) fn apply_with(
    store: &mut CollaborationStore,
    digest: &str,
    options: &GcOptions,
    hooks: &GcHooks,
) -> Result<GcReport, GcError> {
    let Some(_exclusive) = try_exclusive(store)? else {
        return Err(GcError::ArchiveInUse);
    };
    let stored: Option<(String, i64)> = store
        .read_connection()
        .query_row(
            "SELECT body, created_ms FROM gc_plans WHERE digest = ?1",
            [digest],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((body, created_ms)) = stored else {
        return Err(GcError::UnknownPlan {
            digest: digest.to_string(),
        });
    };
    if options.now() - created_ms > PLAN_TTL_MS {
        return Err(GcError::StalePlan {
            digest: digest.to_string(),
        });
    }
    let planned: StoredPlan = serde_json::from_str(&body).map_err(|_| GcError::UnknownPlan {
        digest: digest.to_string(),
    })?;

    // Eligibility now, under the exclusive lock.
    let survey = survey(store, options, None)?;
    if !survey.blockers.is_empty() {
        return Err(GcError::Blocked(survey.blockers));
    }
    let mut reclaim = Vec::new();
    let mut skipped = Vec::new();
    for (kind, relpath, bytes) in planned.items {
        match survey.candidates.get(&relpath) {
            Some(item) if kind_name(item.kind) == kind && item.bytes == bytes => {
                // An object that does not hash to its name is not ours to judge.
                if item.kind == ItemKind::Object {
                    let name = relpath.replace("objects/sha256/", "").replace('/', "");
                    let intact = parse_hex_digest(&name)
                        .is_some_and(|digest| read_object(store, &digest).is_ok());
                    if !intact {
                        skipped.push((item.clone(), "corrupt_object"));
                        continue;
                    }
                }
                reclaim.push(item.clone());
            }
            Some(item) => skipped.push((item.clone(), "changed_since_plan")),
            None => skipped.push((
                GcItem {
                    kind: match kind.as_str() {
                        "spool_temporary" => ItemKind::SpoolTemporary,
                        "operation_lock" => ItemKind::OperationLock,
                        _ => ItemKind::Object,
                    },
                    relpath,
                    bytes,
                    class: RetentionClass::Orphan,
                },
                "no_longer_eligible",
            )),
        }
    }
    let entities: Vec<GcEntity> = planned
        .entities
        .into_iter()
        .map(|(kind, id)| {
            if kind == "snapshot" {
                GcEntity::Snapshot(id)
            } else {
                GcEntity::Contribution(id)
            }
        })
        .filter(|entity| survey.entities.contains(entity))
        .collect();

    // The decision is durable before any file moves.
    let now = now_ms();
    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO gc_generations (plan_digest, state, started_ms) VALUES (?1, 'trashing', ?2)",
        rusqlite::params![digest, now],
    )?;
    let generation = transaction.last_insert_rowid();
    for item in &reclaim {
        transaction.execute(
            "INSERT INTO gc_trash (generation, relpath, bytes) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                generation,
                item.relpath,
                i64::try_from(item.bytes).unwrap_or(i64::MAX)
            ],
        )?;
    }
    for entity in &entities {
        match entity {
            GcEntity::Snapshot(id) => transaction.execute(
                "INSERT OR IGNORE INTO reclaimed_snapshots (snapshot_id, generation, reclaimed_ms)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![id, generation, now],
            )?,
            GcEntity::Contribution(id) => transaction.execute(
                "INSERT OR IGNORE INTO reclaimed_contributions
                     (lineage_record_id, generation, reclaimed_ms)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![id, generation, now],
            )?,
        };
    }
    transaction.execute("DELETE FROM gc_plans WHERE digest = ?1", [digest])?;
    transaction.commit()?;

    let trash = trash_dir(store, generation);
    std::fs::create_dir_all(&trash).map_err(|source| io(&trash, source))?;
    let mut touched = BTreeSet::new();
    for (index, item) in reclaim.iter().enumerate() {
        hooks.moved(index)?;
        move_to_trash(store, generation, &item.relpath, &mut touched)?;
    }
    for dir in touched.iter().chain(std::iter::once(&trash)) {
        sync_directory(dir)?;
    }
    finish_generation(store, generation)?;
    Ok(GcReport {
        generation: Some(generation),
        reclaimed_bytes: reclaim.iter().map(|item| item.bytes).sum(),
        reclaimed: reclaim,
        entities,
        skipped,
    })
}

/// Finish every interrupted generation. Each file is revalidated first: one
/// that something now relies on is put back, the rest are removed.
pub fn resume(
    store: &mut CollaborationStore,
    options: &GcOptions,
) -> Result<Vec<Resumed>, GcError> {
    let Some(_exclusive) = try_exclusive(store)? else {
        return Err(GcError::ArchiveInUse);
    };
    let pending: Vec<(i64, String)> = store
        .read_connection()
        .prepare(
            "SELECT generation, state FROM gc_generations WHERE state != 'done'
             ORDER BY generation",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut resumed = Vec::new();
    for (generation, state) in pending {
        let (mut removed, mut restored) = (0, 0);
        if state == "trashing" {
            // Judge every file as if it were still in place: put moved files
            // back first, then survey.
            let rows: Vec<String> = store
                .read_connection()
                .prepare("SELECT relpath FROM gc_trash WHERE generation = ?1")?
                .query_map([generation], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            let mut moved_back = BTreeSet::new();
            for relpath in &rows {
                let trashed = trash_path(store, generation, relpath);
                let original = store.project_dir().join(relpath);
                if trashed.exists() {
                    if original.exists() {
                        // A later capture wrote it again; the copies are the same.
                        std::fs::remove_file(&trashed).map_err(|source| io(&trashed, source))?;
                    } else {
                        std::fs::rename(&trashed, &original)
                            .map_err(|source| io(&original, source))?;
                        moved_back.insert(relpath.clone());
                    }
                }
            }
            let survey = survey(store, options, Some(generation))?;
            if !survey.blockers.is_empty() {
                return Err(GcError::Blocked(survey.blockers));
            }
            let trash = trash_dir(store, generation);
            std::fs::create_dir_all(&trash).map_err(|source| io(&trash, source))?;
            let mut touched = BTreeSet::new();
            for relpath in &rows {
                if survey.candidates.contains_key(relpath) {
                    move_to_trash(store, generation, relpath, &mut touched)?;
                    removed += 1;
                } else if moved_back.contains(relpath) || store.project_dir().join(relpath).exists()
                {
                    restored += 1;
                }
            }
            for dir in touched.iter().chain(std::iter::once(&trash)) {
                sync_directory(dir)?;
            }
        }
        finish_generation(store, generation)?;
        resumed.push(Resumed {
            generation,
            removed,
            restored,
        });
    }
    Ok(resumed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration_archive::{CommitOid, pin_commit, reconstruct, retained};
    use crate::collaboration_capture::{
        CaptureOutcome, CapturePolicy, CaptureReceipt, CaptureRequest, FaultPoint, Hooks,
        OperationId, RetentionBoundary, capture, capture_with, recover,
    };
    use crate::collaboration_state::{CollaborationRoot, ProjectKey};
    use std::io::BufRead;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    fn git_in(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Base `a`, `b`; result changes `a` and adds `c` (`b` is shared).
    fn repo(tag: &str) -> (tempfile::TempDir, CommitOid, CommitOid) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git_in(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), format!("{tag} base a\n")).unwrap();
        std::fs::write(repo.join("b.txt"), format!("{tag} shared b\n")).unwrap();
        git_in(repo, &["add", "-A"]);
        git_in(repo, &["commit", "-qm", "base"]);
        let base = pin_commit(repo, "HEAD").unwrap();
        std::fs::write(repo.join("a.txt"), format!("{tag} result a\n")).unwrap();
        std::fs::write(repo.join("c.txt"), format!("{tag} result c\n")).unwrap();
        git_in(repo, &["add", "-A"]);
        git_in(repo, &["commit", "-qm", "result"]);
        let result = pin_commit(repo, "HEAD").unwrap();
        (dir, base, result)
    }

    fn open(host: &Path) -> CollaborationStore {
        CollaborationStore::open(
            &CollaborationRoot::under_host_state(host),
            &ProjectKey::parse("proj-a").unwrap(),
            &[],
        )
        .unwrap()
    }

    fn request(
        repo: &Path,
        id: &str,
        base: &CommitOid,
        result: &CommitOid,
        retention: RetentionBoundary,
    ) -> CaptureRequest {
        CaptureRequest {
            operation_id: OperationId::parse(id).unwrap(),
            repository: repo.to_path_buf(),
            base: base.clone(),
            result: result.clone(),
            policy: CapturePolicy::Advisory,
            retention,
        }
    }

    fn captured(store: &mut CollaborationStore, request: &CaptureRequest) -> CaptureReceipt {
        match capture(store, request).unwrap() {
            CaptureOutcome::Acknowledged(receipt) => receipt,
            other => panic!("{other:?}"),
        }
    }

    /// Judge everything as old: a day from now, no grace.
    fn later() -> GcOptions {
        GcOptions {
            grace_ms: 0,
            now_ms: Some(now_ms() + 1000),
        }
    }

    fn objects(plan: &GcPlan) -> Vec<&GcItem> {
        plan.reclaimable
            .iter()
            .filter(|item| item.kind == ItemKind::Object)
            .collect()
    }

    /// INV01: every receipt whose root is live rebuilds both ends.
    fn assert_reconstructs(store: &CollaborationStore, receipt: &CaptureReceipt) {
        for id in [&receipt.base, &receipt.result] {
            let dest = tempfile::tempdir().unwrap();
            let rebuilt = reconstruct(store, id, &dest.path().join("tree")).unwrap();
            assert_eq!(&rebuilt.id(), id);
        }
    }

    fn object_files(store: &CollaborationStore) -> usize {
        std::fs::read_dir(objects_dir(store))
            .map(|dirs| {
                dirs.flatten()
                    .map(|dir| std::fs::read_dir(dir.path()).map_or(0, |files| files.count()))
                    .sum()
            })
            .unwrap_or(0)
    }

    #[test]
    fn rooted_source_is_never_reclaimed_and_every_receipt_reconstructs() {
        let host = tempfile::tempdir().unwrap();
        let (source, base, result) = repo("x");
        let mut store = open(host.path());
        let one = captured(
            &mut store,
            &request(
                source.path(),
                "op-1",
                &base,
                &result,
                RetentionBoundary::UntilReleased,
            ),
        );
        let plan = plan(&mut store, &later()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(objects(&plan).is_empty(), "{:?}", plan.reclaimable);
        assert!(plan.entities.is_empty());
        let retained_classes: Vec<_> = plan.retained.keys().copied().collect();
        assert_eq!(
            retained_classes,
            [
                RetentionClass::Source,
                RetentionClass::Record,
                RetentionClass::Receipt
            ]
        );
        // The ended operation's lock file is the only thing to reclaim.
        assert!(
            plan.reclaimable
                .iter()
                .all(|item| item.kind == ItemKind::OperationLock)
        );
        apply(&mut store, &plan.digest, &later()).unwrap();
        assert_reconstructs(&store, &one);
    }

    #[test]
    fn a_released_contribution_is_reclaimed_as_a_unit_and_can_be_captured_again() {
        let host = tempfile::tempdir().unwrap();
        let (source, base, result) = repo("x");
        let mut store = open(host.path());
        let one = captured(
            &mut store,
            &request(
                source.path(),
                "op-1",
                &base,
                &result,
                RetentionBoundary::UntilReleased,
            ),
        );
        assert!(release_retention(&mut store, "op-1").unwrap());
        let plan = plan(&mut store, &later()).unwrap();
        // 4 blobs, 2 manifests, 2 snapshot records, the lineage and the receipt.
        assert_eq!(objects(&plan).len(), 10, "{:?}", plan.reclaimable);
        assert_eq!(plan.entities.len(), 3);
        let report = apply(&mut store, &plan.digest, &later()).unwrap();
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(object_files(&store), 0);
        for id in [&one.base, &one.result] {
            assert!(retained(&store, id).unwrap().is_none());
            let dest = tempfile::tempdir().unwrap();
            assert_eq!(
                reconstruct(&store, id, dest.path()).unwrap_err().code(),
                "not_retained"
            );
            assert_eq!(
                acquire_lease(
                    &mut store,
                    &LeaseTarget::Snapshot(id.clone()),
                    "t",
                    i64::MAX
                )
                .unwrap_err()
                .code(),
                "not_retained"
            );
        }
        // The same content again: a new receipt, fully retained.
        let two = captured(
            &mut store,
            &request(
                source.path(),
                "op-2",
                &base,
                &result,
                RetentionBoundary::UntilReleased,
            ),
        );
        assert_eq!(two.contribution, one.contribution);
        assert_reconstructs(&store, &two);
        let plan = super::plan(&mut store, &later()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(
            objects(&plan)
                .iter()
                .all(|item| item.class == RetentionClass::Receipt)
        );
    }

    /// T46: another root keeps shared source when one root is released.
    #[test]
    fn releasing_one_root_keeps_what_another_root_reaches() {
        let host = tempfile::tempdir().unwrap();
        let (source, base, result) = repo("x");
        let mut store = open(host.path());
        captured(
            &mut store,
            &request(
                source.path(),
                "op-1",
                &base,
                &result,
                RetentionBoundary::UntilReleased,
            ),
        );
        let two = captured(
            &mut store,
            &request(
                source.path(),
                "op-2",
                &base,
                &result,
                RetentionBoundary::UntilReleased,
            ),
        );
        release_retention(&mut store, "op-1").unwrap();
        let plan = plan(&mut store, &later()).unwrap();
        let classes: Vec<_> = objects(&plan).iter().map(|item| item.class).collect();
        assert_eq!(
            classes,
            [RetentionClass::Receipt],
            "only op-1's receipt goes"
        );
        assert!(plan.entities.is_empty());
        apply(&mut store, &plan.digest, &later()).unwrap();
        assert_reconstructs(&store, &two);
    }

    /// T46: a reader lease keeps source past its root's expiry; releasing the
    /// lease lets it go.
    #[test]
    fn a_live_reader_lease_outlasts_an_expired_root() {
        let host = tempfile::tempdir().unwrap();
        let (source, base, result) = repo("x");
        let mut store = open(host.path());
        let expires = now_ms() + 60_000;
        let receipt = captured(
            &mut store,
            &request(
                source.path(),
                "op-1",
                &base,
                &result,
                RetentionBoundary::UntilMs(expires),
            ),
        );
        let lease = acquire_lease(
            &mut store,
            &LeaseTarget::Snapshot(receipt.result.clone()),
            "reader",
            expires + 3_600_000,
        )
        .unwrap();
        let after_expiry = GcOptions {
            grace_ms: 0,
            now_ms: Some(expires + 1),
        };
        let plan = plan(&mut store, &after_expiry).unwrap();
        // The base snapshot and its unshared blob, the lineage and the receipt.
        assert!(
            plan.entities
                .contains(&GcEntity::Snapshot(receipt.base.to_string()))
        );
        assert!(
            !plan
                .entities
                .contains(&GcEntity::Snapshot(receipt.result.to_string()))
        );
        apply(&mut store, &plan.digest, &after_expiry).unwrap();
        let dest = tempfile::tempdir().unwrap();
        reconstruct(&store, &receipt.result, &dest.path().join("tree")).unwrap();
        assert!(retained(&store, &receipt.base).unwrap().is_none());

        assert!(release_lease(&mut store, lease).unwrap());
        let plan = super::plan(&mut store, &after_expiry).unwrap();
        assert!(
            plan.entities
                .contains(&GcEntity::Snapshot(receipt.result.to_string()))
        );
        apply(&mut store, &plan.digest, &after_expiry).unwrap();
        assert_eq!(object_files(&store), 0);
    }

    #[test]
    fn a_pin_keeps_one_object_as_cited_evidence() {
        let host = tempfile::tempdir().unwrap();
        let (source, base, result) = repo("x");
        let mut store = open(host.path());
        captured(
            &mut store,
            &request(
                source.path(),
                "op-1",
                &base,
                &result,
                RetentionBoundary::UntilReleased,
            ),
        );
        let evidence = ObjectDigest::of(b"x shared b\n");
        pin_object(
            &mut store,
            PinClass::CitedEvidence,
            &evidence,
            "verifier",
            None,
        )
        .unwrap();
        assert_eq!(
            pin_object(
                &mut store,
                PinClass::AnalysisView,
                &ObjectDigest::of(b"never stored"),
                "view",
                None
            )
            .unwrap_err()
            .code(),
            "not_retained"
        );
        release_retention(&mut store, "op-1").unwrap();
        let plan = plan(&mut store, &later()).unwrap();
        assert_eq!(
            plan.retained.get(&RetentionClass::CitedEvidence),
            Some(&ClassTotal {
                objects: 1,
                bytes: 11
            })
        );
        apply(&mut store, &plan.digest, &later()).unwrap();
        assert_eq!(read_object(&store, &evidence).unwrap(), b"x shared b\n");
        assert_eq!(object_files(&store), 1);
    }

    #[test]
    fn young_orphans_and_unrecognised_files_are_protected() {
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let orphan = crate::collaboration_archive::put_object(&store, b"orphan").unwrap();
        let stray_dir = objects_dir(&store).join("zz");
        std::fs::create_dir_all(&stray_dir).unwrap();
        std::fs::write(stray_dir.join("notes.txt"), "mine").unwrap();
        let misnamed = object_path(&store, &orphan).with_file_name("0".repeat(62));
        std::fs::write(&misnamed, "not what the name says").unwrap();

        let fresh = plan(&mut store, &GcOptions::default()).unwrap();
        assert!(fresh.reclaimable.is_empty(), "{:?}", fresh.reclaimable);
        assert!(fresh.protected_bytes > 0);
        assert_eq!(fresh.unknown, ["objects/sha256/zz"]);

        let old = plan(&mut store, &later()).unwrap();
        let paths: Vec<_> = old
            .reclaimable
            .iter()
            .map(|item| item.relpath.clone())
            .collect();
        assert_eq!(paths.len(), 2, "{paths:?}");
        assert!(
            old.reclaimable
                .iter()
                .all(|item| item.class == RetentionClass::Orphan)
        );
        let report = apply(&mut store, &old.digest, &later()).unwrap();
        // A file that does not hash to its name is not reclaimed.
        assert_eq!(report.reclaimed.len(), 1);
        assert_eq!(report.skipped[0].1, "corrupt_object");
        assert!(misnamed.exists());
        assert!(stray_dir.join("notes.txt").exists());
        assert!(!object_path(&store, &orphan).exists());
    }

    /// State that changes between plan and apply wins: newly rooted source is
    /// skipped, never removed.
    #[test]
    fn a_stale_plan_skips_what_became_rooted() {
        let host = tempfile::tempdir().unwrap();
        let (source, base, result) = repo("x");
        let mut store = open(host.path());
        captured(
            &mut store,
            &request(
                source.path(),
                "op-1",
                &base,
                &result,
                RetentionBoundary::UntilReleased,
            ),
        );
        release_retention(&mut store, "op-1").unwrap();
        let plan = plan(&mut store, &later()).unwrap();
        let two = captured(
            &mut store,
            &request(
                source.path(),
                "op-2",
                &base,
                &result,
                RetentionBoundary::UntilReleased,
            ),
        );
        let report = apply(&mut store, &plan.digest, &later()).unwrap();
        assert!(report.entities.is_empty(), "{:?}", report.entities);
        assert_eq!(
            report
                .reclaimed
                .iter()
                .filter(|item| item.kind == ItemKind::Object)
                .map(|item| item.class)
                .collect::<Vec<_>>(),
            [RetentionClass::Receipt]
        );
        assert_eq!(report.skipped.len(), 9, "{:?}", report.skipped);
        assert_reconstructs(&store, &two);
    }

    #[test]
    fn apply_needs_the_archive_to_itself_and_a_current_plan() {
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        crate::collaboration_archive::put_object(&store, b"orphan").unwrap();
        let plan = plan(&mut store, &later()).unwrap();
        {
            let _reader = archive_use(&store).unwrap();
            assert_eq!(
                apply(&mut store, &plan.digest, &later())
                    .unwrap_err()
                    .code(),
                "archive_in_use"
            );
        }
        assert_eq!(
            apply(&mut store, &"0".repeat(64), &later())
                .unwrap_err()
                .code(),
            "unknown_plan"
        );
        let much_later = GcOptions {
            grace_ms: 0,
            now_ms: Some(plan.created_ms + PLAN_TTL_MS + 1),
        };
        assert_eq!(
            apply(&mut store, &plan.digest, &much_later)
                .unwrap_err()
                .code(),
            "stale_plan"
        );
        apply(&mut store, &plan.digest, &later()).unwrap();
    }

    #[test]
    fn a_capture_without_an_outcome_blocks_reclamation_until_recovered() {
        let host = tempfile::tempdir().unwrap();
        let (source, base, result) = repo("x");
        let mut store = open(host.path());
        crate::collaboration_archive::put_object(&store, b"orphan").unwrap();
        let hooks = Hooks {
            fault: Some(FaultPoint::DuringCopy(1)),
            free_bytes: None,
        };
        capture_with(
            &mut store,
            &request(
                source.path(),
                "op-1",
                &base,
                &result,
                RetentionBoundary::UntilReleased,
            ),
            &hooks,
        )
        .unwrap_err();
        let blocked = plan(&mut store, &later()).unwrap();
        assert_eq!(blocked.blockers[0].kind, "unrecovered_capture");
        assert!(blocked.reclaimable.is_empty());
        assert!(blocked.next_action.contains("recover"));
        recover(&mut store).unwrap();
        let plan = plan(&mut store, &later()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert!(!plan.reclaimable.is_empty());
    }

    /// T45: apply fails part way (a full disk, say). The decision is durable,
    /// nothing retained is lost, a capture of the same content meanwhile is
    /// honoured, and resume finishes the rest.
    #[test]
    fn a_failure_mid_apply_resumes_without_losing_retained_source() {
        let host = tempfile::tempdir().unwrap();
        let (one_repo, one_base, one_result) = repo("one");
        let (two_repo, two_base, two_result) = repo("two");
        let mut store = open(host.path());
        captured(
            &mut store,
            &request(
                one_repo.path(),
                "op-1",
                &one_base,
                &one_result,
                RetentionBoundary::UntilReleased,
            ),
        );
        let kept = captured(
            &mut store,
            &request(
                two_repo.path(),
                "op-2",
                &two_base,
                &two_result,
                RetentionBoundary::UntilReleased,
            ),
        );
        release_retention(&mut store, "op-1").unwrap();
        let plan = plan(&mut store, &later()).unwrap();
        let hooks = GcHooks {
            fail_after_moves: Some(3),
            ..GcHooks::default()
        };
        assert_eq!(
            apply_with(&mut store, &plan.digest, &later(), &hooks)
                .unwrap_err()
                .code(),
            "injected"
        );
        assert_reconstructs(&store, &kept);
        let blocked = super::plan(&mut store, &later()).unwrap();
        assert_eq!(blocked.blockers[0].kind, "interrupted_gc");

        // Captured again while the generation is interrupted.
        let again = captured(
            &mut store,
            &request(
                one_repo.path(),
                "op-3",
                &one_base,
                &one_result,
                RetentionBoundary::UntilReleased,
            ),
        );
        let resumed = resume(&mut store, &later()).unwrap();
        assert_eq!(resumed.len(), 1);
        assert!(resumed[0].restored > 0, "{resumed:?}");
        assert_reconstructs(&store, &kept);
        assert_reconstructs(&store, &again);
        assert!(
            super::plan(&mut store, &later())
                .unwrap()
                .blockers
                .is_empty()
        );
    }

    /// A capture that starts while apply holds the archive waits, then writes
    /// its objects afresh: it can never name an object being removed.
    #[test]
    fn a_capture_racing_an_apply_waits_and_keeps_its_source() {
        let host = tempfile::tempdir().unwrap();
        let (source, base, result) = repo("x");
        let mut store = open(host.path());
        captured(
            &mut store,
            &request(
                source.path(),
                "op-1",
                &base,
                &result,
                RetentionBoundary::UntilReleased,
            ),
        );
        release_retention(&mut store, "op-1").unwrap();
        let plan = plan(&mut store, &later()).unwrap();

        let racer = Arc::new(Mutex::new(None));
        let started = Arc::clone(&racer);
        let (host_path, repo_path) = (host.path().to_path_buf(), source.path().to_path_buf());
        let (b, r) = (base.clone(), result.clone());
        let hooks = GcHooks {
            after_moves: Some((
                2,
                Box::new(move || {
                    let (host_path, repo_path, b, r) =
                        (host_path.clone(), repo_path.clone(), b.clone(), r.clone());
                    *started.lock().unwrap() = Some(std::thread::spawn(move || {
                        let mut store = open(&host_path);
                        captured(
                            &mut store,
                            &request(&repo_path, "op-2", &b, &r, RetentionBoundary::UntilReleased),
                        )
                    }));
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }),
            )),
            ..GcHooks::default()
        };
        let report = apply_with(&mut store, &plan.digest, &later(), &hooks).unwrap();
        let receipt = racer.lock().unwrap().take().unwrap().join().unwrap();
        let removed = report
            .reclaimed
            .iter()
            .filter(|item| item.kind == ItemKind::Object)
            .count();
        assert_eq!(removed, 10);
        assert_reconstructs(&store, &receipt);
    }

    /// The archive refreshes an object a capture reuses, so an old orphan
    /// that a capture is about to name is inside the grace period again.
    #[test]
    fn reusing_an_object_restarts_its_grace_period() {
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let digest = crate::collaboration_archive::put_object(&store, b"reused").unwrap();
        let path = object_path(&store, &digest);
        let two_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(172_800);
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(two_days_ago)
            .unwrap();
        assert_eq!(
            plan(&mut store, &GcOptions::default())
                .unwrap()
                .reclaimable
                .len(),
            1
        );
        crate::collaboration_archive::put_object(&store, b"reused").unwrap();
        assert!(
            plan(&mut store, &GcOptions::default())
                .unwrap()
                .reclaimable
                .is_empty()
        );
    }

    /// Reconstruction holds the archive shared, so it waits for an apply
    /// instead of reading a half-removed snapshot.
    #[test]
    fn a_reader_waits_for_an_apply_to_finish() {
        let host = tempfile::tempdir().unwrap();
        let (source, base, result) = repo("x");
        let mut store = open(host.path());
        let receipt = captured(
            &mut store,
            &request(
                source.path(),
                "op-1",
                &base,
                &result,
                RetentionBoundary::UntilReleased,
            ),
        );
        let exclusive = try_exclusive(&store).unwrap().unwrap();
        let (done, finished) = std::sync::mpsc::channel();
        let host_path = host.path().to_path_buf();
        let id = receipt.result.clone();
        let reader = std::thread::spawn(move || {
            let store = open(&host_path);
            let dest = tempfile::tempdir().unwrap();
            let result = reconstruct(&store, &id, &dest.path().join("tree")).map(|_| ());
            done.send(()).unwrap();
            result
        });
        assert!(
            finished
                .recv_timeout(std::time::Duration::from_millis(400))
                .is_err(),
            "the reader did not wait"
        );
        drop(exclusive);
        reader.join().unwrap().unwrap();
    }

    const STALL_CHILD: &str = "AETHYME_GC_STALL_CHILD";

    #[test]
    #[ignore = "child process of a_killed_apply_is_resumed"]
    fn gc_child_stalls_mid_apply() {
        let Some(spec) = std::env::var_os(STALL_CHILD) else {
            return;
        };
        let spec = spec.to_string_lossy().into_owned();
        let (host, digest) = spec.split_once('\n').unwrap();
        let mut store = open(Path::new(host));
        let hooks = GcHooks {
            stall_after_moves: Some(4),
            ..GcHooks::default()
        };
        apply_with(&mut store, digest, &later(), &hooks).unwrap();
    }

    /// An apply killed between moves leaves a durable generation that resume
    /// finishes; live receipts are untouched throughout.
    #[test]
    fn a_killed_apply_is_resumed() {
        let host = tempfile::tempdir().unwrap();
        let (one_repo, one_base, one_result) = repo("one");
        let (two_repo, two_base, two_result) = repo("two");
        let mut store = open(host.path());
        captured(
            &mut store,
            &request(
                one_repo.path(),
                "op-1",
                &one_base,
                &one_result,
                RetentionBoundary::UntilReleased,
            ),
        );
        let kept = captured(
            &mut store,
            &request(
                two_repo.path(),
                "op-2",
                &two_base,
                &two_result,
                RetentionBoundary::UntilReleased,
            ),
        );
        release_retention(&mut store, "op-1").unwrap();
        let plan = plan(&mut store, &later()).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "collaboration_gc::tests::gc_child_stalls_mid_apply",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(
                STALL_CHILD,
                format!("{}\n{}", host.path().display(), plan.digest),
            )
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines() {
            if line.unwrap().contains("stalled") {
                break;
            }
        }
        child.kill().unwrap();
        child.wait().unwrap();

        assert_reconstructs(&store, &kept);
        assert_eq!(
            super::plan(&mut store, &later()).unwrap().blockers[0].kind,
            "interrupted_gc"
        );
        let resumed = resume(&mut store, &later()).unwrap();
        assert_eq!(resumed[0].removed, plan.reclaimable.len(), "{resumed:?}");
        assert_reconstructs(&store, &kept);
        let after = super::plan(&mut store, &later()).unwrap();
        assert!(
            after.blockers.is_empty() && objects(&after).is_empty(),
            "{after:?}"
        );
    }
}
