//! Crash-safe local capture: intent, retained source, record and receipt
//! (#658, plan §6.4, §10.1 CAP, §10.9; D09, D18, D20, D26; INV01).
//!
//! | State | Durable point | After a crash |
//! |---|---|---|
//! | `intent` | Operation row, request digest, byte reservation and a `capture_intent` retention root, in one short transaction. | Retry the same operation, or [`abort`]. Nothing is promised yet. |
//! | `copying` | The archive copies and verifies source (#657); each object is flushed before it is published. | Copied objects are orphans the intent root protects; a retry reuses them. |
//! | `sealed` | Every object is published and indexed; the lineage record ID is on the operation row. | A retry commits without reading the source again, so it completes even if the contributor's repository is gone. |
//! | `committed` | One transaction: receipt row, `contribution` retention root, released intent root, outbox row. | The contribution is recoverable; a retry answers with the same receipt. |
//! | `acknowledged` | Set after the receipt is built for the caller. | Identical to `committed` for every reader; a lost acknowledgement changes nothing. |
//!
//! Failures end the operation in a state that says what a retry can do:
//! `incomplete` (the source was missing or did not match: a fuller clone may
//! succeed), `failed` (a local error such as a full disk: retry), `refused`
//! (nothing a retry changes), or `aborted` (explicit). Only the first two are
//! retried by calling [`capture`] again with the same operation.
//!
//! - **The operation ID is the retry key.** The same ID with the same
//!   request returns the same receipt; the same ID with a different request
//!   is refused. One worker at a time holds a lock file per operation, so
//!   concurrent deliveries of one operation produce exactly one receipt, and
//!   [`recover`] can tell a dead worker from a live one.
//! - **A different ID for the same content** is a separate operation with its
//!   own receipt, retention root and outbox row, naming the same contribution
//!   (the lineage record ID is content-derived). Releasing one operation's
//!   root never removes source another still roots.
//! - **No cross-store transaction.** Objects are published before the commit
//!   transaction that names them; a crash in between leaves orphans, never a
//!   receipt without bytes. Nothing touches `broker.db`, and the outbox row is
//!   only a local intent to tell someone: no delivery is implied (D26).
//! - **Capture is separate from submit.** Nothing here runs from, or changes
//!   the verdict of, legacy submit (integration is #660).
//!
//! **Contract for reclamation (#659).** An archive object may be reclaimed
//! only when no unreleased `retention_roots` row reaches it (a
//! `capture_intent` root protects everything an operation in `intent`,
//! `copying` or `sealed` may have written, including unindexed orphans), no
//! operation is in one of those states, and the operation's lock file is not
//! held.

use std::path::{Path, PathBuf};

use aethyme_contracts::experimental_v0::canonical_json::{Object, Value};
use aethyme_contracts::experimental_v0::{
    FieldKind, FieldSpec, Record, RecordId, RecordSchema, SourceSnapshotId,
};
use rusqlite::{OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::collaboration_archive::{
    ArchiveError, CommitOid, ObjectDigest, RetainedContribution, git as archive_git, has_object,
    put_object, refuse_partial_clone, retain_contribution_with,
};
use crate::collaboration_state::{CollaborationStateError, CollaborationStore};
use crate::file_lock::{ExclusiveFileLock, open_lock_file};

pub const CAPTURE_RECEIPT_SCHEMA_NAME: &str = "aethyme.capture-receipt/experimental-v0";

const fn field(name: &'static str, kind: FieldKind) -> FieldSpec {
    FieldSpec {
        name,
        required: true,
        kind,
        capability: None,
    }
}

/// A local capture receipt. It names the operation, the contribution and its
/// two snapshots, what the receipt promises and for how long.
pub static CAPTURE_RECEIPT_SCHEMA: RecordSchema = RecordSchema {
    name: CAPTURE_RECEIPT_SCHEMA_NAME,
    fields: &[
        field("operation", FieldKind::String),
        field("status", FieldKind::String),
        field("durability", FieldKind::String),
        field("contribution", FieldKind::String),
        field("base", FieldKind::String),
        field("result", FieldKind::String),
        field("retention", FieldKind::Opaque),
    ],
    capabilities: &[],
};

/// Free space a capture must leave on the store's filesystem after its own
/// and every active reservation.
pub const RESERVATION_FLOOR_BYTES: u64 = 256 * 1024 * 1024;
/// Added to each estimate for manifests, records and filesystem overhead.
const RESERVATION_OVERHEAD_BYTES: u64 = 1024 * 1024;

/// The retry key of one capture. Opaque: 1-128 ASCII letters, digits, `.`,
/// `_`, `:` or `-`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OperationId(String);

impl OperationId {
    pub fn parse(text: &str) -> Result<Self, CaptureError> {
        let valid = (1..=128).contains(&text.len())
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'));
        if valid {
            Ok(Self(text.to_string()))
        } else {
            Err(CaptureError::InvalidOperationId {
                text: text.to_string(),
            })
        }
    }

    /// A new random ID (`cap-` and 128 random bits).
    pub fn mint() -> Result<Self, CaptureError> {
        use std::io::Read;
        let mut bytes = [0u8; 16];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut bytes))
            .map_err(|source| CaptureError::Io {
                path: PathBuf::from("/dev/urandom"),
                source,
            })?;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Ok(Self(format!("cap-{hex}")))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether a failed capture may stop the workflow that asked for it. Only
/// recorded here: #660 decides what each policy does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePolicy {
    Advisory,
    Required,
}

impl CapturePolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::Advisory => "advisory",
            Self::Required => "required",
        }
    }
}

/// How long acknowledged source is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionBoundary {
    /// Until an explicit release (#659).
    UntilReleased,
    /// Until this time (milliseconds since the Unix epoch).
    UntilMs(i64),
}

impl RetentionBoundary {
    fn until_ms(self) -> Option<i64> {
        match self {
            Self::UntilReleased => None,
            Self::UntilMs(ms) => Some(ms),
        }
    }

    fn from_until_ms(value: Option<i64>) -> Self {
        value.map_or(Self::UntilReleased, Self::UntilMs)
    }

    fn describe(self) -> String {
        match self {
            Self::UntilReleased => "until_released".into(),
            Self::UntilMs(ms) => format!("until_ms:{ms}"),
        }
    }
}

/// One capture request. The repository is a locator, not part of identity:
/// the commits are.
#[derive(Debug, Clone)]
pub struct CaptureRequest {
    pub operation_id: OperationId,
    pub repository: PathBuf,
    pub base: CommitOid,
    pub result: CommitOid,
    pub policy: CapturePolicy,
    pub retention: RetentionBoundary,
}

impl CaptureRequest {
    fn digest(&self) -> String {
        let canonical = format!(
            "aethyme capture request v0\0{}\0{}\0{}\0{}",
            self.base.as_str(),
            self.result.as_str(),
            self.policy.as_str(),
            self.retention.describe()
        );
        Sha256::digest(canonical.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

/// What an acknowledged capture promises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureReceipt {
    pub operation_id: OperationId,
    /// `retained_local` while a live retention root holds the contribution:
    /// the bytes and record survived the store's durability profile on this
    /// host (never an off-host promise). `released` once the operation's
    /// root is released or past its boundary, and `reclaimed` once
    /// reclamation (#659) removed its source; neither still promises source.
    pub status: &'static str,
    /// The store's receipt label: `local_durable` only on the supported
    /// profile, `local_unverified` otherwise.
    pub durability: String,
    /// The contribution's lineage record ID.
    pub contribution: RecordId,
    pub base: SourceSnapshotId,
    pub result: SourceSnapshotId,
    pub retention: RetentionBoundary,
    /// The ID of the receipt record stored in the archive.
    pub receipt_record_id: RecordId,
}

/// The outcome of a capture that did not fail locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    Acknowledged(CaptureReceipt),
    /// The source was missing or did not match. Nothing was promised; a retry
    /// with the same operation, from a fuller clone, may succeed.
    Incomplete {
        operation_id: OperationId,
        code: String,
        detail: String,
    },
}

/// What [`recover`] did with one operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovery {
    pub operation_id: OperationId,
    pub from: String,
    pub to: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error(
        "{text:?} is not a valid operation ID: use 1-128 ASCII letters, digits, '.', '_', ':' or '-'"
    )]
    InvalidOperationId { text: String },
    #[error(
        "operation {operation_id} was already used for a different request; use a new \
         operation ID for new inputs"
    )]
    ConflictingOperation { operation_id: String },
    #[error(
        "capture needs about {needed} bytes but only {available} are free and {reserved} are \
         reserved by other captures, which would leave less than the {floor}-byte floor; free \
         space or wait for active captures to finish"
    )]
    InsufficientSpace {
        needed: u64,
        available: u64,
        reserved: u64,
        floor: u64,
    },
    #[error("capture {operation_id} was refused ({code}): {detail}")]
    Refused {
        operation_id: String,
        code: String,
        detail: String,
    },
    #[error("capture {operation_id} failed ({code}): {detail}; retry the same operation")]
    Failed {
        operation_id: String,
        code: String,
        detail: String,
    },
    #[error("capture {operation_id} was aborted")]
    Aborted { operation_id: String },
    #[error(
        "capture {operation_id} is committed; releasing its source is reclamation's job (#659)"
    )]
    AlreadyCommitted { operation_id: String },
    #[error("no capture operation {operation_id}")]
    UnknownOperation { operation_id: String },
    #[error("capture {operation_id} has a receipt that is inconsistent with the store: {detail}")]
    CorruptReceipt {
        operation_id: String,
        detail: String,
    },
    #[cfg(test)]
    #[error("injected fault at {0:?}")]
    Injected(FaultPoint),
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

impl CaptureError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidOperationId { .. } => "invalid_operation_id",
            Self::ConflictingOperation { .. } => "conflicting_operation",
            Self::InsufficientSpace { .. } => "insufficient_space",
            Self::Refused { .. } => "refused",
            Self::Failed { .. } => "failed",
            Self::Aborted { .. } => "aborted",
            Self::AlreadyCommitted { .. } => "already_committed",
            Self::UnknownOperation { .. } => "unknown_operation",
            Self::CorruptReceipt { .. } => "corrupt_receipt",
            #[cfg(test)]
            Self::Injected(_) => "injected",
            Self::Io { .. } => "io",
            Self::Sqlite(_) => "sqlite",
            Self::State(_) => "state",
        }
    }
}

/// Where a test stops a capture as if the process had died there.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    AfterIntent,
    /// Before the n-th blob of the base snapshot is copied.
    DuringCopy(usize),
    /// Every object is published, before the operation is marked sealed.
    AfterCopy,
    AfterSealed,
    /// Inside the commit transaction, before it commits.
    BeforeCommit,
    /// The transaction committed; the caller never hears back.
    AfterCommit,
    /// The disk fills while the n-th blob is copied.
    NoSpaceDuringCopy(usize),
}

/// Test seams. Production passes the default.
#[derive(Default)]
pub(crate) struct Hooks {
    #[cfg(test)]
    pub fault: Option<FaultPoint>,
    /// Free bytes to report instead of asking the filesystem.
    pub free_bytes: Option<u64>,
}

impl Hooks {
    #[cfg(test)]
    fn hit(&self, point: FaultPoint) -> Result<(), CaptureError> {
        if self.fault == Some(point) {
            return Err(CaptureError::Injected(point));
        }
        Ok(())
    }

    #[cfg(not(test))]
    #[inline]
    fn hit(&self, _point: ()) -> Result<(), CaptureError> {
        Ok(())
    }
}

#[cfg(test)]
macro_rules! fault {
    ($hooks:expr, $point:expr) => {
        $hooks.hit($point)?
    };
}

#[cfg(not(test))]
macro_rules! fault {
    ($hooks:expr, $point:expr) => {
        $hooks.hit(())?
    };
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

pub(crate) fn locks_dir(store: &CollaborationStore) -> PathBuf {
    store.project_dir().join("spool/capture")
}

/// Hold the operation's lock: blocking for a capture, so concurrent
/// deliveries run one after another; non-blocking for recovery.
fn lock_operation(
    store: &CollaborationStore,
    operation_id: &OperationId,
    wait: bool,
) -> Result<Option<ExclusiveFileLock>, CaptureError> {
    let dir = locks_dir(store);
    std::fs::create_dir_all(&dir).map_err(|source| CaptureError::Io {
        path: dir.clone(),
        source,
    })?;
    crate::host_state::protect_host_state_path(&dir, true).map_err(|source| CaptureError::Io {
        path: dir.clone(),
        source,
    })?;
    let path = dir.join(format!("{}.lock", operation_id.as_str()));
    let io = |source| CaptureError::Io {
        path: path.clone(),
        source,
    };
    loop {
        let file = open_lock_file(&path).map_err(io)?;
        let held = file.try_clone().map_err(io)?;
        let lock = if wait {
            ExclusiveFileLock::acquire(file).map_err(io)?
        } else {
            match ExclusiveFileLock::try_acquire(file).map_err(io)? {
                Some(lock) => lock,
                None => return Ok(None),
            }
        };
        // The holder may have unlinked the path (allowed once the operation
        // is final) while this caller waited on the old file. A lock on an
        // inode the path no longer names excludes nobody: start again.
        if same_file(&held, &path) {
            return Ok(Some(lock));
        }
    }
}

/// Whether the open `file` is the one `path` names now.
fn same_file(file: &std::fs::File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    match (file.metadata(), std::fs::metadata(path)) {
        (Ok(open), Ok(named)) => open.dev() == named.dev() && open.ino() == named.ino(),
        _ => false,
    }
}

/// Hold the archive shared (see `collaboration_gc`).
fn archive_use(
    store: &CollaborationStore,
) -> Result<crate::collaboration_gc::ArchiveUse, CaptureError> {
    crate::collaboration_gc::archive_use(store).map_err(|source| CaptureError::Io {
        path: crate::collaboration_gc::lock_path(store),
        source,
    })
}

struct OperationRow {
    request_digest: String,
    state: String,
    lineage_record_id: Option<String>,
    outcome_code: Option<String>,
    outcome_detail: Option<String>,
}

fn load_operation(
    connection: &rusqlite::Connection,
    operation_id: &OperationId,
) -> Result<Option<OperationRow>, CaptureError> {
    Ok(connection
        .query_row(
            "SELECT request_digest, state, lineage_record_id, outcome_code, outcome_detail
             FROM capture_operations WHERE operation_id = ?1",
            [operation_id.as_str()],
            |row| {
                Ok(OperationRow {
                    request_digest: row.get(0)?,
                    state: row.get(1)?,
                    lineage_record_id: row.get(2)?,
                    outcome_code: row.get(3)?,
                    outcome_detail: row.get(4)?,
                })
            },
        )
        .optional()?)
}

/// Stored and reported failure details come from Git's stderr, which the
/// repository controls: keep at most this many bytes.
const MAX_DETAIL_BYTES: usize = 4096;

fn bounded(detail: &str) -> String {
    if detail.len() <= MAX_DETAIL_BYTES {
        return detail.to_string();
    }
    let mut end = MAX_DETAIL_BYTES;
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated]", &detail[..end])
}

fn set_state(
    store: &mut CollaborationStore,
    operation_id: &OperationId,
    state: &str,
    outcome: Option<(&str, &str)>,
) -> Result<(), CaptureError> {
    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "UPDATE capture_operations
         SET state = ?2, outcome_code = ?3, outcome_detail = ?4, updated_ms = ?5
         WHERE operation_id = ?1",
        rusqlite::params![
            operation_id.as_str(),
            state,
            outcome.map(|(code, _)| code),
            outcome.map(|(_, detail)| bounded(detail)),
            now_ms()
        ],
    )?;
    // A terminal failure gives up the intent root and the reservation: an
    // operation that is not running protects nothing.
    if matches!(state, "incomplete" | "failed" | "refused" | "aborted") {
        transaction.execute(
            "UPDATE retention_roots SET released_ms = ?2
             WHERE operation_id = ?1 AND kind = 'capture_intent' AND released_ms IS NULL",
            rusqlite::params![operation_id.as_str(), now_ms()],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

/// Bytes a capture of `request` may write: every blob of both trees, before
/// deduplication, plus overhead. Runs Git exactly as the archive does (no
/// lazy fetch, no prompts, no caller-supplied repository variables), after
/// refusing a partial clone, so estimating cannot fetch or read another
/// repository. A tree Git cannot list is an error, never a small estimate.
fn estimate_bytes(request: &CaptureRequest) -> Result<u64, ArchiveError> {
    refuse_partial_clone(&request.repository)?;
    let mut total = RESERVATION_OVERHEAD_BYTES;
    for commit in [&request.base, &request.result] {
        let output = archive_git(&request.repository)
            .args(["ls-tree", "-r", "-l", "-z", "--full-tree", commit.as_str()])
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|error| ArchiveError::Git {
                detail: format!("could not run git ls-tree: {error}"),
            })?;
        if !output.status.success() {
            return Err(ArchiveError::SourceUnavailable {
                oid: commit.as_str().to_string(),
                detail: format!(
                    "git ls-tree could not list the tree: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        for record in output.stdout.split(|b| *b == 0) {
            // "<mode> <type> <oid> <size>\t<path>"
            let header = record.split(|b| *b == b'\t').next().unwrap_or_default();
            if let Some(size) = String::from_utf8_lossy(header)
                .split_whitespace()
                .nth(3)
                .and_then(|size| size.parse::<u64>().ok())
            {
                total = total.saturating_add(size);
            }
        }
    }
    Ok(total)
}

fn free_bytes(path: &Path) -> Result<u64, CaptureError> {
    use std::os::unix::ffi::OsStrExt;

    let io = |source| CaptureError::Io {
        path: path.to_path_buf(),
        source,
    };
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io(std::io::Error::from(std::io::ErrorKind::InvalidInput)))?;
    // SAFETY: statvfs is a plain struct of integers, for which all-zero is
    // valid; the call only writes into it.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is NUL-terminated and `stat` is writable.
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return Err(io(std::io::Error::last_os_error()));
    }
    #[allow(clippy::unnecessary_cast)]
    Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

/// What [`begin`] found or recorded.
enum Begun {
    Row(OperationRow),
    /// The preflight found the source incomplete; nothing was reserved.
    Incomplete(CaptureOutcome),
}

/// Record a pre-intent failure on an existing row (a retry), or nothing for
/// a new operation, and classify it like a copy failure.
fn preflight_failure(
    store: &mut CollaborationStore,
    request: &CaptureRequest,
    existing: bool,
    error: ArchiveError,
) -> Result<Begun, CaptureError> {
    let (state, outcome) = if error.is_incomplete() {
        ("incomplete", Ok(()))
    } else if is_refusal(&error) {
        (
            "refused",
            Err(CaptureError::Refused {
                operation_id: request.operation_id.as_str().to_string(),
                code: error.code().to_string(),
                detail: bounded(&error.to_string()),
            }),
        )
    } else {
        (
            "failed",
            Err(archive_failure(&request.operation_id, &error)),
        )
    };
    if existing {
        set_state(
            store,
            &request.operation_id,
            state,
            Some((error.code(), &error.to_string())),
        )?;
    }
    outcome.map(|()| {
        Begun::Incomplete(CaptureOutcome::Incomplete {
            operation_id: request.operation_id.clone(),
            code: error.code().to_string(),
            detail: bounded(&error.to_string()),
        })
    })
}

/// CAP_INTENT: record the operation, or find it. Returns the existing row
/// when this operation was seen before. A source that cannot be estimated is
/// answered before anything is reserved; a new operation then leaves no row.
fn begin(
    store: &mut CollaborationStore,
    request: &CaptureRequest,
    hooks: &Hooks,
) -> Result<Begun, CaptureError> {
    let digest = request.digest();
    let existing = load_operation(store.connection(), &request.operation_id)?;
    if let Some(row) = &existing
        && row.request_digest != digest
    {
        return Err(CaptureError::ConflictingOperation {
            operation_id: request.operation_id.as_str().to_string(),
        });
    }
    let resumable = match existing.as_ref().map(|row| row.state.as_str()) {
        None => true,
        Some("intent" | "copying" | "incomplete" | "failed") => true,
        // Sealed resumes at commit; the terminal states are answered as they are.
        Some(_) => false,
    };
    if !resumable {
        return Ok(Begun::Row(
            existing.expect("only an existing row is not resumable"),
        ));
    }

    let needed = match estimate_bytes(request) {
        Ok(needed) => needed,
        Err(error) => return preflight_failure(store, request, existing.is_some(), error),
    };
    // A crashed capture's reservation must not starve this one until someone
    // calls `recover`: resolve every other in-flight operation no live
    // worker holds (non-blocking, so a live one is skipped).
    reap_crashed(store, Some(&request.operation_id));
    let available = match hooks.free_bytes {
        Some(bytes) => bytes,
        None => free_bytes(store.project_dir())?,
    };
    let operation = request.operation_id.as_str();
    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let reserved: i64 = transaction.query_row(
        "SELECT COALESCE(SUM(reserved_bytes), 0) FROM capture_operations
         WHERE state IN ('intent', 'copying', 'sealed') AND operation_id != ?1",
        [operation],
        |row| row.get(0),
    )?;
    let reserved = u64::try_from(reserved).unwrap_or(0);
    if available.saturating_sub(reserved).saturating_sub(needed) < RESERVATION_FLOOR_BYTES {
        return Err(CaptureError::InsufficientSpace {
            needed,
            available,
            reserved,
            floor: RESERVATION_FLOOR_BYTES,
        });
    }
    let now = now_ms();
    let needed_i64 = i64::try_from(needed).unwrap_or(i64::MAX);
    if existing.is_none() {
        transaction.execute(
            "INSERT INTO capture_operations
                 (operation_id, request_digest, repository, base_commit, result_commit, policy,
                  retention_until_ms, reserved_bytes, state, created_ms, updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'intent', ?9, ?9)",
            rusqlite::params![
                operation,
                digest,
                request.repository.to_string_lossy(),
                request.base.as_str(),
                request.result.as_str(),
                request.policy.as_str(),
                request.retention.until_ms(),
                needed_i64,
                now,
            ],
        )?;
    } else {
        transaction.execute(
            "UPDATE capture_operations
             SET state = 'intent', reserved_bytes = ?2, outcome_code = NULL,
                 outcome_detail = NULL, updated_ms = ?3
             WHERE operation_id = ?1",
            rusqlite::params![operation, needed_i64, now],
        )?;
    }
    let has_intent_root: bool = transaction
        .query_row(
            "SELECT 1 FROM retention_roots WHERE operation_id = ?1
             AND kind = 'capture_intent' AND released_ms IS NULL",
            [operation],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !has_intent_root {
        transaction.execute(
            "INSERT INTO retention_roots (kind, operation_id, created_ms)
             VALUES ('capture_intent', ?1, ?2)",
            rusqlite::params![operation, now],
        )?;
    }
    transaction.commit()?;
    fault!(hooks, FaultPoint::AfterIntent);
    Ok(Begun::Row(
        load_operation(store.connection(), &request.operation_id)?
            .expect("the operation row was just written"),
    ))
}

fn record_id(text: &str, operation_id: &OperationId) -> Result<RecordId, CaptureError> {
    RecordId::parse(text).map_err(|_| CaptureError::CorruptReceipt {
        operation_id: operation_id.as_str().to_string(),
        detail: format!("{text:?} is not a record ID"),
    })
}

fn snapshot_id(text: &str, operation_id: &OperationId) -> Result<SourceSnapshotId, CaptureError> {
    SourceSnapshotId::parse(text).map_err(|_| CaptureError::CorruptReceipt {
        operation_id: operation_id.as_str().to_string(),
        detail: format!("{text:?} is not a snapshot ID"),
    })
}

/// The receipt committed for `operation_id`, if any.
pub fn receipt(
    store: &mut CollaborationStore,
    operation_id: &OperationId,
) -> Result<Option<CaptureReceipt>, CaptureError> {
    let row = store
        .connection()
        .query_row(
            "SELECT r.receipt_record_id, r.lineage_record_id, r.base_snapshot, r.result_snapshot,
                    r.durability, o.retention_until_ms
             FROM capture_receipts r JOIN capture_operations o USING (operation_id)
             WHERE r.operation_id = ?1",
            [operation_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                ))
            },
        )
        .optional()?;
    let Some((receipt_id, lineage, base, result, durability, until)) = row else {
        return Ok(None);
    };
    let reclaimed: bool = store.connection().query_row(
        "SELECT EXISTS (SELECT 1 FROM reclaimed_contributions WHERE lineage_record_id = ?1)
             OR EXISTS (SELECT 1 FROM reclaimed_snapshots WHERE snapshot_id IN (?2, ?3))",
        rusqlite::params![lineage, base, result],
        |row| row.get(0),
    )?;
    let live_root: bool = store.connection().query_row(
        "SELECT EXISTS (SELECT 1 FROM retention_roots
             WHERE operation_id = ?1 AND kind = 'contribution' AND released_ms IS NULL
               AND (until_ms IS NULL OR until_ms > ?2))",
        rusqlite::params![operation_id.as_str(), now_ms()],
        |row| row.get(0),
    )?;
    let status = if reclaimed {
        "reclaimed"
    } else if live_root {
        "retained_local"
    } else {
        "released"
    };
    Ok(Some(CaptureReceipt {
        operation_id: operation_id.clone(),
        status,
        durability,
        contribution: record_id(&lineage, operation_id)?,
        base: snapshot_id(&base, operation_id)?,
        result: snapshot_id(&result, operation_id)?,
        retention: RetentionBoundary::from_until_ms(until),
        receipt_record_id: record_id(&receipt_id, operation_id)?,
    }))
}

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

fn object(members: Vec<(&str, Value)>) -> Value {
    Value::Object(
        Object::new(
            members
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
        .expect("distinct keys"),
    )
}

fn receipt_record(
    operation_id: &OperationId,
    durability: &str,
    lineage: &str,
    base: &str,
    result: &str,
    retention: RetentionBoundary,
) -> (Vec<u8>, RecordId) {
    let retention = match retention {
        RetentionBoundary::UntilReleased => object(vec![("kind", text("until_released"))]),
        RetentionBoundary::UntilMs(ms) => object(vec![
            ("kind", text("until")),
            ("until_ms", text(&ms.to_string())),
        ]),
    };
    let bytes = object(vec![
        ("schema", text(CAPTURE_RECEIPT_SCHEMA_NAME)),
        ("operation", text(operation_id.as_str())),
        ("status", text("retained_local")),
        ("durability", text(durability)),
        ("contribution", text(lineage)),
        ("base", text(base)),
        ("result", text(result)),
        ("retention", retention),
    ])
    .to_canonical_bytes();
    let id = Record::decode(&bytes, &[&CAPTURE_RECEIPT_SCHEMA])
        .expect("receipts are valid by construction")
        .id();
    (bytes, id)
}

/// CAP_COMMITTED: one transaction for the receipt, the contribution root, the
/// released intent root and the outbox row. The receipt record object is
/// published first, so a committed receipt never names missing bytes.
fn commit(
    store: &mut CollaborationStore,
    request: &CaptureRequest,
    lineage: &str,
    hooks: &Hooks,
) -> Result<CaptureReceipt, CaptureError> {
    let operation_id = &request.operation_id;
    let (base, result): (String, String) = store
        .connection()
        .query_row(
            "SELECT base_snapshot, result_snapshot FROM retained_contributions
             WHERE lineage_record_id = ?1",
            [lineage],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| CaptureError::CorruptReceipt {
            operation_id: operation_id.as_str().to_string(),
            detail: format!("sealed contribution {lineage} is not in the archive index"),
        })?;
    // Sealed means retained. The index rows are guaranteed by the foreign
    // keys; check that the manifests themselves are still on disk.
    for snapshot in [&base, &result] {
        let id = snapshot_id(snapshot, operation_id)?;
        if !has_object(store, &ObjectDigest::of_snapshot(&id)) {
            return Err(CaptureError::CorruptReceipt {
                operation_id: operation_id.as_str().to_string(),
                detail: format!("the manifest of snapshot {snapshot} is missing from the archive"),
            });
        }
    }
    let durability = store.durability().receipt_label().to_string();
    let (bytes, receipt_id) = receipt_record(
        operation_id,
        &durability,
        lineage,
        &base,
        &result,
        request.retention,
    );
    let digest =
        put_object(store, &bytes).map_err(|error| archive_failure(operation_id, &error))?;

    let now = now_ms();
    let operation = operation_id.as_str();
    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let already: bool = transaction
        .query_row(
            "SELECT 1 FROM capture_receipts WHERE operation_id = ?1",
            [operation],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if !already {
        transaction.execute(
            "INSERT INTO capture_receipts
                 (operation_id, receipt_record_id, receipt_sha256, lineage_record_id,
                  base_snapshot, result_snapshot, durability, committed_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                operation,
                receipt_id.as_str(),
                digest.hex(),
                lineage,
                base,
                result,
                durability,
                now
            ],
        )?;
        transaction.execute(
            "INSERT INTO retention_roots (kind, operation_id, lineage_record_id, until_ms, created_ms)
             VALUES ('contribution', ?1, ?2, ?3, ?4)",
            rusqlite::params![operation, lineage, request.retention.until_ms(), now],
        )?;
        transaction.execute(
            "UPDATE retention_roots SET released_ms = ?2
             WHERE operation_id = ?1 AND kind = 'capture_intent' AND released_ms IS NULL",
            rusqlite::params![operation, now],
        )?;
        transaction.execute(
            "INSERT INTO outbox (operation_id, kind, payload_record_id, created_ms)
             VALUES (?1, 'capture_receipt', ?2, ?3)",
            rusqlite::params![operation, receipt_id.as_str(), now],
        )?;
        transaction.execute(
            "UPDATE capture_operations
             SET state = 'committed', lineage_record_id = ?2, outcome_code = NULL,
                 outcome_detail = NULL, updated_ms = ?3
             WHERE operation_id = ?1",
            rusqlite::params![operation, lineage, now],
        )?;
    }
    fault!(hooks, FaultPoint::BeforeCommit);
    transaction.commit()?;
    fault!(hooks, FaultPoint::AfterCommit);
    receipt(store, operation_id)?.ok_or_else(|| CaptureError::CorruptReceipt {
        operation_id: operation.to_string(),
        detail: "the committed receipt is missing".into(),
    })
}

fn archive_failure(operation_id: &OperationId, error: &ArchiveError) -> CaptureError {
    CaptureError::Failed {
        operation_id: operation_id.as_str().to_string(),
        code: error.code().to_string(),
        detail: bounded(&error.to_string()),
    }
}

/// An archive error that no retry changes: the request names something a
/// v0 snapshot or contribution cannot be.
fn is_refusal(error: &ArchiveError) -> bool {
    matches!(
        error,
        ArchiveError::NotAnObjectId { .. }
            | ArchiveError::NotACommit { .. }
            | ArchiveError::UnknownRevision { .. }
            | ArchiveError::UnsupportedEntry { .. }
            | ArchiveError::UnsupportedFilter { .. }
            | ArchiveError::InvalidSnapshot(_)
            | ArchiveError::BaseNotAncestor { .. }
            // The same commit always produces it.
            | ArchiveError::MalformedSource { .. }
    )
}

/// Answer an operation already past copying, without touching the source.
fn answer(
    store: &mut CollaborationStore,
    request: &CaptureRequest,
    row: &OperationRow,
    hooks: &Hooks,
) -> Result<CaptureOutcome, CaptureError> {
    let operation = request.operation_id.as_str().to_string();
    let detail = || row.outcome_detail.clone().unwrap_or_default();
    let code = || row.outcome_code.clone().unwrap_or_default();
    match row.state.as_str() {
        "committed" | "acknowledged" => acknowledge(store, &request.operation_id),
        "sealed" => {
            let lineage =
                row.lineage_record_id
                    .clone()
                    .ok_or_else(|| CaptureError::CorruptReceipt {
                        operation_id: operation.clone(),
                        detail: "a sealed operation has no contribution".into(),
                    })?;
            commit_or_fail(store, request, &lineage, hooks)?;
            acknowledge(store, &request.operation_id)
        }
        "refused" => Err(CaptureError::Refused {
            operation_id: operation,
            code: code(),
            detail: detail(),
        }),
        "aborted" => Err(CaptureError::Aborted {
            operation_id: operation,
        }),
        other => Err(CaptureError::CorruptReceipt {
            operation_id: operation,
            detail: format!("unexpected state {other}"),
        }),
    }
}

/// CAP_ACKNOWLEDGED: mark it and return the receipt. Marking is not a
/// promise: a crash before it leaves `committed`, which answers the same.
fn acknowledge(
    store: &mut CollaborationStore,
    operation_id: &OperationId,
) -> Result<CaptureOutcome, CaptureError> {
    let receipt = receipt(store, operation_id)?.ok_or_else(|| CaptureError::CorruptReceipt {
        operation_id: operation_id.as_str().to_string(),
        detail: "no receipt for a committed operation".into(),
    })?;
    store.connection().execute(
        "UPDATE capture_operations SET state = 'acknowledged', updated_ms = ?2
         WHERE operation_id = ?1 AND state = 'committed'",
        rusqlite::params![operation_id.as_str(), now_ms()],
    )?;
    Ok(CaptureOutcome::Acknowledged(receipt))
}

/// Capture `request`, or answer it again if it was captured before.
pub fn capture(
    store: &mut CollaborationStore,
    request: &CaptureRequest,
) -> Result<CaptureOutcome, CaptureError> {
    capture_with(store, request, &Hooks::default())
}

pub(crate) fn capture_with(
    store: &mut CollaborationStore,
    request: &CaptureRequest,
    hooks: &Hooks,
) -> Result<CaptureOutcome, CaptureError> {
    // Shared with every other capture and reader; reclamation (#659) waits
    // for all of them. Taken before the operation lock, always.
    let _use = archive_use(store)?;
    let _lock = lock_operation(store, &request.operation_id, true)?;
    let row = match begin(store, request, hooks)? {
        Begun::Row(row) => row,
        Begun::Incomplete(outcome) => return Ok(outcome),
    };
    if row.state != "intent" {
        return answer(store, request, &row, hooks);
    }

    set_state(store, &request.operation_id, "copying", None)?;
    #[cfg(test)]
    let mut before_blob = |index: usize| -> Result<(), ArchiveError> {
        match hooks.fault {
            Some(FaultPoint::DuringCopy(at)) if at == index => Err(ArchiveError::Git {
                detail: "injected crash during copy".into(),
            }),
            Some(FaultPoint::NoSpaceDuringCopy(at)) if at == index => Err(ArchiveError::Io {
                path: store_spool_hint(),
                source: std::io::Error::from_raw_os_error(libc::ENOSPC),
            }),
            _ => Ok(()),
        }
    };
    #[cfg(not(test))]
    let mut before_blob = |_: usize| -> Result<(), ArchiveError> { Ok(()) };
    let retained = retain_contribution_with(
        store,
        &request.repository,
        &request.base,
        &request.result,
        &mut before_blob,
    );
    #[cfg(test)]
    if matches!(hooks.fault, Some(FaultPoint::DuringCopy(_))) && retained.is_err() {
        // A crash leaves the operation where it was; no outcome is recorded.
        return Err(CaptureError::Injected(hooks.fault.expect("matched")));
    }
    let contribution: RetainedContribution = match retained {
        Ok(contribution) => contribution,
        Err(error) if error.is_incomplete() => {
            set_state(
                store,
                &request.operation_id,
                "incomplete",
                Some((error.code(), &error.to_string())),
            )?;
            return Ok(CaptureOutcome::Incomplete {
                operation_id: request.operation_id.clone(),
                code: error.code().to_string(),
                detail: bounded(&error.to_string()),
            });
        }
        Err(error) if is_refusal(&error) => {
            set_state(
                store,
                &request.operation_id,
                "refused",
                Some((error.code(), &error.to_string())),
            )?;
            return Err(CaptureError::Refused {
                operation_id: request.operation_id.as_str().to_string(),
                code: error.code().to_string(),
                detail: bounded(&error.to_string()),
            });
        }
        Err(error) => {
            set_state(
                store,
                &request.operation_id,
                "failed",
                Some((error.code(), &error.to_string())),
            )?;
            return Err(archive_failure(&request.operation_id, &error));
        }
    };
    fault!(hooks, FaultPoint::AfterCopy);

    // CAP_OBJECTS_SEALED.
    let lineage = contribution.lineage_record_id.as_str().to_string();
    store.connection().execute(
        "UPDATE capture_operations SET state = 'sealed', lineage_record_id = ?2, updated_ms = ?3
         WHERE operation_id = ?1",
        rusqlite::params![request.operation_id.as_str(), lineage, now_ms()],
    )?;
    fault!(hooks, FaultPoint::AfterSealed);
    commit(store, request, &lineage, hooks)?;
    acknowledge(store, &request.operation_id)
}

#[cfg(test)]
fn store_spool_hint() -> PathBuf {
    PathBuf::from("spool/archive")
}

/// Commit a sealed operation. If its retained bytes are gone the commit can
/// never succeed: the operation becomes `failed` (`corrupt_receipt`), which
/// releases its reservation and intent root, and a retry copies again.
fn commit_or_fail(
    store: &mut CollaborationStore,
    request: &CaptureRequest,
    lineage: &str,
    hooks: &Hooks,
) -> Result<CaptureReceipt, CaptureError> {
    match commit(store, request, lineage, hooks) {
        Err(error @ CaptureError::CorruptReceipt { .. }) => {
            set_state(
                store,
                &request.operation_id,
                "failed",
                Some(("corrupt_receipt", &error.to_string())),
            )?;
            Err(error)
        }
        other => other,
    }
}

/// What [`recover`] did: the operations it resolved, and those it could not.
#[derive(Debug, Default)]
pub struct RecoveryReport {
    pub recovered: Vec<Recovery>,
    pub errors: Vec<(OperationId, CaptureError)>,
}

/// Resolve one in-flight operation if no live worker holds it.
fn recover_one(
    store: &mut CollaborationStore,
    operation_id: &OperationId,
) -> Result<Option<Recovery>, CaptureError> {
    let Some(_lock) = lock_operation(store, operation_id, false)? else {
        return Ok(None); // A live worker owns it.
    };
    // Re-read under the lock: the worker may have finished meanwhile.
    let Some(row) = load_operation(store.connection(), operation_id)? else {
        return Ok(None);
    };
    let to = match row.state.as_str() {
        "intent" | "copying" => {
            set_state(
                store,
                operation_id,
                "failed",
                Some((
                    "interrupted",
                    "the capturing process stopped before sealing",
                )),
            )?;
            "failed"
        }
        "sealed" => {
            let request = stored_request(store, operation_id)?;
            let Some(lineage) = row.lineage_record_id.clone() else {
                set_state(
                    store,
                    operation_id,
                    "failed",
                    Some(("corrupt_receipt", "a sealed operation has no contribution")),
                )?;
                return Ok(Some(Recovery {
                    operation_id: operation_id.clone(),
                    from: row.state,
                    to: "failed".into(),
                }));
            };
            match commit_or_fail(store, &request, &lineage, &Hooks::default()) {
                Ok(_) => "committed",
                Err(CaptureError::CorruptReceipt { .. }) => "failed",
                Err(error) => return Err(error),
            }
        }
        _ => return Ok(None),
    };
    Ok(Some(Recovery {
        operation_id: operation_id.clone(),
        from: row.state,
        to: to.to_string(),
    }))
}

fn in_flight(store: &mut CollaborationStore) -> Result<Vec<OperationId>, CaptureError> {
    let ids: Vec<String> = store
        .connection()
        .prepare(
            "SELECT operation_id FROM capture_operations
             WHERE state IN ('intent', 'copying', 'sealed') ORDER BY operation_id",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    ids.iter().map(|id| OperationId::parse(id)).collect()
}

/// Resolve crashed operations other than `except`. A failure is printed as a
/// warning and does not stop the capture: an operation that cannot be
/// resolved now is left for [`recover`] to report.
fn reap_crashed(store: &mut CollaborationStore, except: Option<&OperationId>) {
    let Ok(ids) = in_flight(store) else { return };
    for id in ids.iter().filter(|id| Some(*id) != except) {
        crate::warn_unrecorded(
            &format!("resolve crashed capture {}", id.as_str()),
            recover_one(store, id),
        );
    }
}

/// After a crash: resolve every operation no live worker holds. `intent` and
/// `copying` become `failed` (code `interrupted`, retryable); `sealed` is
/// committed, because its source is already retained, or becomes `failed`
/// (`corrupt_receipt`) if those bytes are gone; `committed` and the terminal
/// states are left alone. One operation's error does not stop the others.
pub fn recover(store: &mut CollaborationStore) -> Result<RecoveryReport, CaptureError> {
    // Shared with captures, exclusive with a GC apply (#659).
    let _use = archive_use(store)?;
    let mut report = RecoveryReport::default();
    for operation_id in in_flight(store)? {
        match recover_one(store, &operation_id) {
            Ok(Some(recovery)) => report.recovered.push(recovery),
            Ok(None) => {}
            Err(error) => report.errors.push((operation_id, error)),
        }
    }
    Ok(report)
}

fn stored_request(
    store: &mut CollaborationStore,
    operation_id: &OperationId,
) -> Result<CaptureRequest, CaptureError> {
    let (repository, base, result, policy, until): (String, String, String, String, Option<i64>) =
        store.connection().query_row(
            "SELECT repository, base_commit, result_commit, policy, retention_until_ms
             FROM capture_operations WHERE operation_id = ?1",
            [operation_id.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
    let commit =
        |text: &str| CommitOid::parse(text).map_err(|error| archive_failure(operation_id, &error));
    Ok(CaptureRequest {
        operation_id: operation_id.clone(),
        repository: PathBuf::from(repository),
        base: commit(&base)?,
        result: commit(&result)?,
        policy: if policy == "required" {
            CapturePolicy::Required
        } else {
            CapturePolicy::Advisory
        },
        retention: RetentionBoundary::from_until_ms(until),
    })
}

/// Abandon an operation that has not committed, releasing its reservation
/// and intent root. A committed one is refused: its source is released by
/// reclamation (#659), not by abort.
pub fn abort(
    store: &mut CollaborationStore,
    operation_id: &OperationId,
) -> Result<(), CaptureError> {
    let _lock = lock_operation(store, operation_id, true)?;
    let row = load_operation(store.connection(), operation_id)?.ok_or_else(|| {
        CaptureError::UnknownOperation {
            operation_id: operation_id.as_str().to_string(),
        }
    })?;
    match row.state.as_str() {
        "committed" | "acknowledged" => Err(CaptureError::AlreadyCommitted {
            operation_id: operation_id.as_str().to_string(),
        }),
        "aborted" => Ok(()),
        _ => set_state(store, operation_id, "aborted", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration_archive::{pin_commit, reconstruct};
    use crate::collaboration_state::{CollaborationRoot, ProjectKey};
    use std::io::BufRead;
    use std::process::Command;

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
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A repository with a base commit and a result commit on top of it.
    fn repo() -> (tempfile::TempDir, CommitOid, CommitOid) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git_in(repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "base a\n").unwrap();
        std::fs::write(repo.join("b.txt"), "base b\n").unwrap();
        git_in(repo, &["add", "-A"]);
        git_in(repo, &["commit", "-qm", "base"]);
        let base = pin_commit(repo, "HEAD").unwrap();
        std::fs::write(repo.join("a.txt"), "result a\n").unwrap();
        std::fs::write(repo.join("c.txt"), "result c\n").unwrap();
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

    fn request(repo: &Path, id: &str, base: &CommitOid, result: &CommitOid) -> CaptureRequest {
        CaptureRequest {
            operation_id: OperationId::parse(id).unwrap(),
            repository: repo.to_path_buf(),
            base: base.clone(),
            result: result.clone(),
            policy: CapturePolicy::Advisory,
            retention: RetentionBoundary::UntilReleased,
        }
    }

    fn acknowledged(outcome: CaptureOutcome) -> CaptureReceipt {
        match outcome {
            CaptureOutcome::Acknowledged(receipt) => receipt,
            other => panic!("expected a receipt, got {other:?}"),
        }
    }

    fn faulted(fault: FaultPoint) -> Hooks {
        Hooks {
            fault: Some(fault),
            free_bytes: None,
        }
    }

    fn count(store: &mut CollaborationStore, sql: &str) -> i64 {
        store
            .connection()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }

    fn state(store: &mut CollaborationStore, id: &str) -> String {
        store
            .connection()
            .query_row(
                "SELECT state FROM capture_operations WHERE operation_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn live_roots(store: &mut CollaborationStore, kind: &str) -> i64 {
        store
            .connection()
            .query_row(
                "SELECT count(*) FROM retention_roots WHERE kind = ?1 AND released_ms IS NULL",
                [kind],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// INV01: every acknowledged receipt rebuilds both ends from the archive.
    fn assert_reconstructs(store: &CollaborationStore, receipt: &CaptureReceipt) {
        for id in [&receipt.base, &receipt.result] {
            let dest = tempfile::tempdir().unwrap();
            let rebuilt = reconstruct(store, id, &dest.path().join("tree")).unwrap();
            assert_eq!(&rebuilt.id(), id);
        }
    }

    /// The receipt a clean run produces for this operation; deterministic.
    fn reference_receipt(
        repo: &Path,
        id: &str,
        base: &CommitOid,
        result: &CommitOid,
    ) -> CaptureReceipt {
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        acknowledged(capture(&mut store, &request(repo, id, base, result)).unwrap())
    }

    #[test]
    fn operation_ids_are_opaque_retry_keys() {
        for good in ["a", "cap-0f", "op:1.2_3", &"x".repeat(128)] {
            assert!(OperationId::parse(good).is_ok(), "{good}");
        }
        for bad in ["", "a b", "a/b", "é", &"x".repeat(129)] {
            assert_eq!(
                OperationId::parse(bad).unwrap_err().code(),
                "invalid_operation_id"
            );
        }
        let minted = OperationId::mint().unwrap();
        assert!(minted.as_str().starts_with("cap-") && minted.as_str().len() == 36);
        assert_ne!(minted, OperationId::mint().unwrap());
    }

    #[test]
    fn a_capture_commits_receipt_root_and_outbox_together() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let receipt = acknowledged(
            capture(&mut store, &request(repo.path(), "op-1", &base, &result)).unwrap(),
        );
        assert_eq!(receipt.status, "retained_local");
        assert_eq!(receipt.durability, store.durability().receipt_label());
        assert_eq!(receipt.retention, RetentionBoundary::UntilReleased);
        assert_eq!(state(&mut store, "op-1"), "acknowledged");
        assert_eq!(
            count(&mut store, "SELECT count(*) FROM capture_receipts"),
            1
        );
        assert_eq!(count(&mut store, "SELECT count(*) FROM outbox"), 1);
        assert_eq!(live_roots(&mut store, "contribution"), 1);
        assert_eq!(
            live_roots(&mut store, "capture_intent"),
            0,
            "the intent root is released"
        );
        assert_eq!(
            count(
                &mut store,
                "SELECT COALESCE(SUM(reserved_bytes), 0) FROM capture_operations
                 WHERE state IN ('intent', 'copying', 'sealed')"
            ),
            0,
            "no reservation outlives its capture"
        );
        assert_reconstructs(&store, &receipt);

        // T10: the contributor's repository is gone; the receipt still holds.
        drop(repo);
        assert_reconstructs(&store, &receipt);
    }

    #[test]
    fn a_retry_returns_the_same_receipt_and_a_changed_request_is_refused() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let first = acknowledged(
            capture(&mut store, &request(repo.path(), "op-1", &base, &result)).unwrap(),
        );
        let again = acknowledged(
            capture(&mut store, &request(repo.path(), "op-1", &base, &result)).unwrap(),
        );
        assert_eq!(first, again);
        assert_eq!(
            count(&mut store, "SELECT count(*) FROM capture_receipts"),
            1
        );
        assert_eq!(count(&mut store, "SELECT count(*) FROM outbox"), 1);

        let mut changed = request(repo.path(), "op-1", &base, &result);
        changed.retention = RetentionBoundary::UntilMs(1);
        assert_eq!(
            capture(&mut store, &changed).unwrap_err().code(),
            "conflicting_operation"
        );
        // The repository path is a locator, not part of the request.
        let moved = request(Path::new("/elsewhere"), "op-1", &base, &result);
        assert_eq!(acknowledged(capture(&mut store, &moved).unwrap()), first);
    }

    /// A second operation for the same content is its own receipt and root,
    /// naming the same contribution.
    #[test]
    fn another_operation_for_the_same_content_names_the_same_contribution() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let one = acknowledged(
            capture(&mut store, &request(repo.path(), "op-1", &base, &result)).unwrap(),
        );
        let two = acknowledged(
            capture(&mut store, &request(repo.path(), "op-2", &base, &result)).unwrap(),
        );
        assert_eq!(one.contribution, two.contribution);
        assert_ne!(one.receipt_record_id, two.receipt_record_id);
        assert_eq!(live_roots(&mut store, "contribution"), 2);
        assert_eq!(count(&mut store, "SELECT count(*) FROM outbox"), 2);
    }

    /// T08: a crash at every state boundary recovers to the receipt a clean
    /// run produces, exactly once, with its bytes.
    #[test]
    fn a_crash_at_every_boundary_recovers_to_one_receipt_with_its_bytes() {
        let (repo, base, result) = repo();
        let reference = reference_receipt(repo.path(), "op-1", &base, &result);
        for fault in [
            FaultPoint::AfterIntent,
            FaultPoint::DuringCopy(0),
            FaultPoint::DuringCopy(1),
            FaultPoint::AfterCopy,
            FaultPoint::AfterSealed,
            FaultPoint::BeforeCommit,
            FaultPoint::AfterCommit,
        ] {
            let host = tempfile::tempdir().unwrap();
            let request = request(repo.path(), "op-1", &base, &result);
            {
                let mut store = open(host.path());
                let error = capture_with(&mut store, &request, &faulted(fault)).unwrap_err();
                assert_eq!(error.code(), "injected", "{fault:?}");
                let interrupted = state(&mut store, "op-1");
                if matches!(interrupted.as_str(), "intent" | "copying" | "sealed") {
                    assert_eq!(
                        live_roots(&mut store, "capture_intent"),
                        1,
                        "{fault:?}: an in-flight capture protects what it wrote"
                    );
                }
                // A receipt exists exactly when the commit landed.
                assert_eq!(
                    receipt(&mut store, &request.operation_id)
                        .unwrap()
                        .is_some(),
                    fault == FaultPoint::AfterCommit,
                    "{fault:?}"
                );
            }
            // Restart: recover, then deliver the same request again.
            let mut store = open(host.path());
            recover(&mut store).unwrap();
            assert!(
                !matches!(
                    state(&mut store, "op-1").as_str(),
                    "intent" | "copying" | "sealed"
                ),
                "{fault:?}: recovery leaves nothing in flight"
            );
            let receipt = acknowledged(capture(&mut store, &request).unwrap());
            assert_eq!(receipt, reference, "{fault:?}");
            assert_eq!(
                count(&mut store, "SELECT count(*) FROM capture_receipts"),
                1
            );
            assert_eq!(
                count(&mut store, "SELECT count(*) FROM outbox"),
                1,
                "{fault:?}"
            );
            assert_eq!(live_roots(&mut store, "contribution"), 1, "{fault:?}");
            assert_eq!(live_roots(&mut store, "capture_intent"), 0, "{fault:?}");
            assert_reconstructs(&store, &receipt);
        }
    }

    /// Sealed means retained: recovery commits it even after the
    /// contributor's repository is deleted.
    #[test]
    fn a_sealed_capture_commits_after_its_source_is_gone() {
        let (repo, base, result) = repo();
        let reference = reference_receipt(repo.path(), "op-1", &base, &result);
        let host = tempfile::tempdir().unwrap();
        let request = request(repo.path(), "op-1", &base, &result);
        let mut store = open(host.path());
        capture_with(&mut store, &request, &faulted(FaultPoint::AfterSealed)).unwrap_err();
        drop(repo);
        let actions = recover(&mut store).unwrap().recovered;
        assert_eq!(actions.len(), 1);
        assert_eq!(
            (actions[0].from.as_str(), actions[0].to.as_str()),
            ("sealed", "committed")
        );
        let receipt = receipt(&mut store, &request.operation_id).unwrap().unwrap();
        assert_eq!(receipt, reference);
        assert_reconstructs(&store, &receipt);

        // The same through a retried delivery instead of recovery.
        let (repo, base, result) = self::repo();
        let host = tempfile::tempdir().unwrap();
        let request = self::request(repo.path(), "op-1", &base, &result);
        let mut store = open(host.path());
        capture_with(&mut store, &request, &faulted(FaultPoint::AfterSealed)).unwrap_err();
        drop(repo);
        let receipt = acknowledged(capture(&mut store, &request).unwrap());
        assert_reconstructs(&store, &receipt);
    }

    /// The commit transaction is idempotent on its own: committing a sealed
    /// operation twice still leaves one receipt, root and outbox row.
    #[test]
    fn committing_twice_adds_nothing() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let request = request(repo.path(), "op-1", &base, &result);
        let mut store = open(host.path());
        capture_with(&mut store, &request, &faulted(FaultPoint::AfterSealed)).unwrap_err();
        let lineage: String = store
            .connection()
            .query_row(
                "SELECT lineage_record_id FROM capture_operations WHERE operation_id = 'op-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let first = commit(&mut store, &request, &lineage, &Hooks::default()).unwrap();
        let second = commit(&mut store, &request, &lineage, &Hooks::default()).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            count(&mut store, "SELECT count(*) FROM capture_receipts"),
            1
        );
        assert_eq!(count(&mut store, "SELECT count(*) FROM outbox"), 1);
        assert_eq!(live_roots(&mut store, "contribution"), 1);
    }

    /// A sealed operation whose retained manifest has gone is not committed.
    #[test]
    fn a_sealed_capture_missing_its_bytes_is_never_committed() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let request = request(repo.path(), "op-1", &base, &result);
        let mut store = open(host.path());
        capture_with(&mut store, &request, &faulted(FaultPoint::AfterSealed)).unwrap_err();
        let result_snapshot: String = store
            .connection()
            .query_row(
                "SELECT result_snapshot FROM retained_contributions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let digest =
            ObjectDigest::of_snapshot(&SourceSnapshotId::parse(&result_snapshot).unwrap()).hex();
        std::fs::remove_file(
            store
                .project_dir()
                .join("objects/sha256")
                .join(&digest[..2])
                .join(&digest[2..]),
        )
        .unwrap();
        assert_eq!(
            capture(&mut store, &request).unwrap_err().code(),
            "corrupt_receipt"
        );
        assert!(
            receipt(&mut store, &request.operation_id)
                .unwrap()
                .is_none()
        );
        // It can never commit, so it stops protecting and reserving; a retry
        // copies the source again and completes.
        assert_eq!(state(&mut store, "op-1"), "failed");
        assert_eq!(live_roots(&mut store, "capture_intent"), 0);
        let receipt = acknowledged(capture(&mut store, &request).unwrap());
        assert_reconstructs(&store, &receipt);
    }

    /// A receipt from an unsupported filesystem never reads like one from
    /// the supported profile.
    #[test]
    fn an_unsupported_filesystem_labels_its_receipt_differently() {
        let (repo, base, result) = repo();
        let supported = reference_receipt(repo.path(), "op-1", &base, &result);
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        store.set_durability_for_test(crate::collaboration_state::DurabilityProfile {
            filesystem: "exfat".into(),
            local: true,
            journal_mode: "wal".into(),
            synchronous: 2,
            full_fsync: true,
            supported: false,
            limitation: Some("exfat is not a supported filesystem".into()),
        });
        let receipt = acknowledged(
            capture(&mut store, &request(repo.path(), "op-1", &base, &result)).unwrap(),
        );
        assert_eq!(receipt.durability, "local_unverified");
        assert_ne!(receipt.receipt_record_id, supported.receipt_record_id);
        if store_is_supported() {
            assert_eq!(supported.durability, "local_durable");
        }
    }

    fn store_is_supported() -> bool {
        let host = tempfile::tempdir().unwrap();
        open(host.path()).durability().supported
    }

    fn no_rows(store: &mut CollaborationStore) -> bool {
        count(store, "SELECT count(*) FROM capture_operations") == 0
    }

    fn incomplete_code(outcome: CaptureOutcome) -> String {
        match outcome {
            CaptureOutcome::Incomplete { code, .. } => code,
            other => panic!("expected incomplete, got {other:?}"),
        }
    }

    /// A partial clone is answered before estimating, so the estimate never
    /// asks Git for blobs the clone lacks; nothing is reserved or recorded.
    #[test]
    fn a_partial_clone_is_answered_before_anything_is_reserved() {
        let (source, base, result) = repo();
        git_in(source.path(), &["config", "uploadpack.allowfilter", "true"]);
        let clone = tempfile::tempdir().unwrap();
        let url = format!("file://{}", source.path().display());
        git_in(
            source.path(),
            &[
                "clone",
                "-q",
                "--filter=blob:none",
                "--no-checkout",
                &url,
                clone.path().to_str().unwrap(),
            ],
        );
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let outcome = capture(&mut store, &request(clone.path(), "op-1", &base, &result)).unwrap();
        assert_eq!(incomplete_code(outcome), "partial_clone");
        assert!(no_rows(&mut store));
    }

    /// A tree Git cannot list is not a small estimate that slips past
    /// admission: it is answered before anything is reserved.
    #[test]
    fn a_tree_git_cannot_list_is_not_a_small_estimate() {
        let (repo, base, _) = repo();
        let missing = CommitOid::parse(&"ab".repeat(20)).unwrap();
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let outcome = capture(&mut store, &request(repo.path(), "op-1", &base, &missing)).unwrap();
        assert_eq!(incomplete_code(outcome), "source_unavailable");
        assert!(no_rows(&mut store));
    }

    const ENV_CHILD: &str = "AETHYME_CAPTURE_ENV_CHILD";

    #[test]
    #[ignore = "child process of caller_git_variables_do_not_redirect_the_capture"]
    fn env_child_captures_with_hostile_git_variables() {
        let Some(spec) = std::env::var_os(ENV_CHILD) else {
            return;
        };
        let spec = spec.to_string_lossy().into_owned();
        let parts: Vec<&str> = spec.split('\n').collect();
        let mut store = open(Path::new(parts[0]));
        let request = request(
            Path::new(parts[1]),
            "op-env",
            &CommitOid::parse(parts[2]).unwrap(),
            &CommitOid::parse(parts[3]).unwrap(),
        );
        match capture(&mut store, &request) {
            Ok(CaptureOutcome::Acknowledged(_)) => println!("RESULT ok"),
            Ok(CaptureOutcome::Incomplete { code, .. }) => println!("RESULT incomplete {code}"),
            Err(error) => println!("RESULT err {}", error.code()),
        }
    }

    /// A caller's `GIT_DIR` or object-directory variables never point the
    /// estimate (or the copy) at another repository.
    #[test]
    fn caller_git_variables_do_not_redirect_the_capture() {
        let (repo, base, result) = repo();
        let (other, _, _) = self::repo();
        let host = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "collaboration_capture::tests::env_child_captures_with_hostile_git_variables",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(
                ENV_CHILD,
                format!(
                    "{}\n{}\n{}\n{}",
                    host.path().display(),
                    repo.path().display(),
                    base.as_str(),
                    result.as_str()
                ),
            )
            .env("GIT_DIR", other.path().join(".git"))
            .env("GIT_OBJECT_DIRECTORY", other.path().join(".git/objects"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("RESULT ok"), "{stdout}");
    }

    /// One operation recovery cannot resolve does not stop the others.
    #[test]
    fn recovery_continues_past_an_operation_it_cannot_resolve() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let broken = request(repo.path(), "op-a", &base, &result);
        capture_with(&mut store, &broken, &faulted(FaultPoint::AfterSealed)).unwrap_err();
        store
            .connection()
            .execute(
                "UPDATE capture_operations SET base_commit = 'not-an-oid'
                 WHERE operation_id = 'op-a'",
                [],
            )
            .unwrap();
        let crashed = request(repo.path(), "op-b", &base, &result);
        capture_with(&mut store, &crashed, &faulted(FaultPoint::AfterIntent)).unwrap_err();

        let report = recover(&mut store).unwrap();
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].0.as_str(), "op-a");
        assert_eq!(report.recovered.len(), 1);
        assert_eq!(report.recovered[0].operation_id.as_str(), "op-b");
        assert_eq!(state(&mut store, "op-b"), "failed");
    }

    /// Recovery moves a sealed operation whose bytes are gone to a terminal
    /// state, releasing what it held.
    #[test]
    fn recovery_fails_a_sealed_operation_whose_bytes_are_gone() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let request = request(repo.path(), "op-1", &base, &result);
        let mut store = open(host.path());
        capture_with(&mut store, &request, &faulted(FaultPoint::AfterSealed)).unwrap_err();
        let manifest: String = store
            .connection()
            .query_row(
                "SELECT base_snapshot FROM retained_contributions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let digest = ObjectDigest::of_snapshot(&SourceSnapshotId::parse(&manifest).unwrap()).hex();
        std::fs::remove_file(
            store
                .project_dir()
                .join("objects/sha256")
                .join(&digest[..2])
                .join(&digest[2..]),
        )
        .unwrap();
        let report = recover(&mut store).unwrap();
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.recovered[0].to, "failed");
        assert_eq!(live_roots(&mut store, "capture_intent"), 0);
        assert_eq!(
            count(
                &mut store,
                "SELECT COALESCE(SUM(reserved_bytes), 0) FROM capture_operations
                 WHERE state IN ('intent', 'copying', 'sealed')"
            ),
            0
        );
    }

    /// A crashed capture's reservation does not starve the next capture
    /// until someone calls `recover`.
    #[test]
    fn a_crashed_reservation_is_reaped_by_the_next_capture() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let first = request(repo.path(), "op-1", &base, &result);
        capture_with(&mut store, &first, &faulted(FaultPoint::AfterIntent)).unwrap_err();
        let room_for_one = Hooks {
            free_bytes: Some(RESERVATION_FLOOR_BYTES + estimate_bytes(&first).unwrap()),
            ..Hooks::default()
        };
        let second = request(repo.path(), "op-2", &base, &result);
        acknowledged(capture_with(&mut store, &second, &room_for_one).unwrap());
        assert_eq!(state(&mut store, "op-1"), "failed");
    }

    #[test]
    fn malformed_source_is_a_refusal() {
        assert!(is_refusal(&ArchiveError::MalformedSource {
            oid: "ab".repeat(20),
            kind: "commit",
        }));
    }

    /// A transforming attribute from this clone's own configuration is
    /// retryable from another clone; one the commit sets is refused for good.
    #[test]
    fn a_clone_local_filter_is_retryable_and_a_committed_one_is_not() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let request = request(repo.path(), "op-1", &base, &result);
        std::fs::write(
            repo.path().join(".git/info/attributes"),
            "*.txt filter=lfs\n",
        )
        .unwrap();
        assert_eq!(
            incomplete_code(capture(&mut store, &request).unwrap()),
            "unsupported_filter"
        );
        std::fs::remove_file(repo.path().join(".git/info/attributes")).unwrap();
        acknowledged(capture(&mut store, &request).unwrap());

        std::fs::write(repo.path().join(".gitattributes"), "*.txt filter=lfs\n").unwrap();
        git_in(repo.path(), &["add", "-A"]);
        git_in(repo.path(), &["commit", "-qm", "filter"]);
        let filtered = pin_commit(repo.path(), "HEAD").unwrap();
        let committed = self::request(repo.path(), "op-2", &base, &filtered);
        for _ in 0..2 {
            match capture(&mut store, &committed).unwrap_err() {
                CaptureError::Refused { code, .. } => assert_eq!(code, "unsupported_filter"),
                other => panic!("{other}"),
            }
        }
    }

    /// Failure details come from repository-controlled Git output.
    #[test]
    fn stored_failure_details_are_bounded() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let request = request(repo.path(), "op-1", &base, &result);
        capture_with(&mut store, &request, &faulted(FaultPoint::AfterIntent)).unwrap_err();
        let huge = "é".repeat(100_000);
        set_state(
            &mut store,
            &request.operation_id,
            "failed",
            Some(("git", &huge)),
        )
        .unwrap();
        let stored: String = store
            .connection()
            .query_row(
                "SELECT outcome_detail FROM capture_operations WHERE operation_id = 'op-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stored.len() <= MAX_DETAIL_BYTES + 32, "{}", stored.len());
        assert!(stored.ends_with("[truncated]"));
    }

    /// A lock path unlinked and recreated while a waiter blocks on the old
    /// file never yields two holders.
    #[test]
    fn an_unlinked_lock_path_cannot_produce_two_holders() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let host = tempfile::tempdir().unwrap();
        let store = open(host.path());
        let id = OperationId::parse("op-1").unwrap();
        let path = locks_dir(&store).join("op-1.lock");
        let holder = lock_operation(&store, &id, false).unwrap().unwrap();

        let acquired = Arc::new(AtomicBool::new(false));
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let waiter = {
            let acquired = Arc::clone(&acquired);
            let host = host.path().to_path_buf();
            let id = id.clone();
            std::thread::spawn(move || {
                let store = open(&host);
                let lock = lock_operation(&store, &id, true).unwrap().unwrap();
                acquired.store(true, Ordering::SeqCst);
                release_rx.recv().unwrap();
                drop(lock);
            })
        };
        std::thread::sleep(Duration::from_millis(300));
        // The holder unlinks the path (allowed once the operation is final);
        // a newcomer creates a new file there and locks it.
        std::fs::remove_file(&path).unwrap();
        let newcomer = lock_operation(&store, &id, false).unwrap().unwrap();
        drop(holder);
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            !acquired.load(Ordering::SeqCst),
            "the waiter holds the old file while the newcomer holds the new one"
        );
        drop(newcomer);
        for _ in 0..100 {
            if acquired.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            acquired.load(Ordering::SeqCst),
            "the waiter never acquired the lock"
        );
        release_tx.send(()).unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn recovery_leaves_an_operation_a_live_worker_holds() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let request = request(repo.path(), "op-1", &base, &result);
        let mut store = open(host.path());
        capture_with(&mut store, &request, &faulted(FaultPoint::AfterIntent)).unwrap_err();
        let held = lock_operation(&store, &request.operation_id, false)
            .unwrap()
            .unwrap();
        let mut other = open(host.path());
        assert!(recover(&mut other).unwrap().recovered.is_empty());
        assert_eq!(state(&mut other, "op-1"), "intent");
        drop(held);
        assert_eq!(recover(&mut other).unwrap().recovered.len(), 1);
        assert_eq!(state(&mut other, "op-1"), "failed");
    }

    /// T07: a branch moving after the commits were pinned changes nothing; a
    /// blob missing at copy time is incomplete, and a retry from a repaired
    /// clone completes.
    #[test]
    fn a_moved_branch_is_ignored_and_a_missing_blob_is_incomplete() {
        let (repo, base, result) = repo();
        let reference = reference_receipt(repo.path(), "op-1", &base, &result);
        std::fs::write(repo.path().join("a.txt"), "moved on\n").unwrap();
        git_in(repo.path(), &["commit", "-qam", "later"]);
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let request = request(repo.path(), "op-1", &base, &result);
        assert_eq!(
            acknowledged(capture(&mut store, &request).unwrap()),
            reference
        );

        // A fresh store and a blob only the result has, removed from the clone.
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let oid = git_in(
            repo.path(),
            &["rev-parse", &format!("{}:c.txt", result.as_str())],
        );
        let loose = repo
            .path()
            .join(".git/objects")
            .join(&oid[..2])
            .join(&oid[2..]);
        let saved = std::fs::read(&loose).unwrap();
        std::fs::remove_file(&loose).unwrap();
        match capture(&mut store, &request).unwrap() {
            CaptureOutcome::Incomplete { code, .. } => assert_eq!(code, "source_unavailable"),
            other => panic!("{other:?}"),
        }
        assert_eq!(state(&mut store, "op-1"), "incomplete");
        assert!(
            receipt(&mut store, &request.operation_id)
                .unwrap()
                .is_none()
        );
        assert_eq!(live_roots(&mut store, "capture_intent"), 0);
        std::fs::write(&loose, saved).unwrap();
        assert_eq!(
            acknowledged(capture(&mut store, &request).unwrap()),
            reference
        );
    }

    /// T45 (partial): reservations count against free space, and a full disk
    /// mid-copy is a retryable failure, never a partial success.
    #[test]
    fn reservations_refuse_before_writing_and_a_full_disk_is_retryable() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let tiny = Hooks {
            free_bytes: Some(RESERVATION_FLOOR_BYTES),
            ..Hooks::default()
        };
        let first = request(repo.path(), "op-1", &base, &result);
        assert_eq!(
            capture_with(&mut store, &first, &tiny).unwrap_err().code(),
            "insufficient_space"
        );
        assert_eq!(
            count(&mut store, "SELECT count(*) FROM capture_operations"),
            0
        );

        // One in-flight capture's reservation leaves no room for a second.
        let estimate = estimate_bytes(&first).unwrap();
        capture_with(&mut store, &first, &faulted(FaultPoint::AfterIntent)).unwrap_err();
        // Its worker is still alive: it holds the operation's lock.
        let live = lock_operation(&store, &first.operation_id, false)
            .unwrap()
            .unwrap();
        let room_for_one = Hooks {
            free_bytes: Some(RESERVATION_FLOOR_BYTES + 2 * estimate - 1),
            ..Hooks::default()
        };
        let second = request(repo.path(), "op-2", &base, &result);
        assert_eq!(
            capture_with(&mut store, &second, &room_for_one)
                .unwrap_err()
                .code(),
            "insufficient_space"
        );
        drop(live);
        abort(&mut store, &first.operation_id).unwrap();
        acknowledged(capture_with(&mut store, &second, &room_for_one).unwrap());

        // ENOSPC while copying.
        let third = request(repo.path(), "op-3", &base, &result);
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let error = capture_with(
            &mut store,
            &third,
            &faulted(FaultPoint::NoSpaceDuringCopy(1)),
        )
        .unwrap_err();
        assert_eq!(error.code(), "failed");
        assert_eq!(state(&mut store, "op-3"), "failed");
        assert_eq!(
            count(&mut store, "SELECT count(*) FROM capture_receipts"),
            0
        );
        assert_eq!(
            count(&mut store, "SELECT count(*) FROM retained_contributions"),
            0
        );
        let receipt = acknowledged(capture(&mut store, &third).unwrap());
        assert_reconstructs(&store, &receipt);
    }

    #[test]
    fn refusals_and_aborts_are_final() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        // Reversed: the base is not an ancestor of the result.
        let backwards = request(repo.path(), "op-r", &result, &base);
        assert_eq!(
            capture(&mut store, &backwards).unwrap_err().code(),
            "refused"
        );
        assert_eq!(state(&mut store, "op-r"), "refused");
        match capture(&mut store, &backwards).unwrap_err() {
            CaptureError::Refused { code, .. } => assert_eq!(code, "base_not_ancestor"),
            other => panic!("{other}"),
        }

        let pending = request(repo.path(), "op-a", &base, &result);
        capture_with(&mut store, &pending, &faulted(FaultPoint::AfterCopy)).unwrap_err();
        abort(&mut store, &pending.operation_id).unwrap();
        assert_eq!(live_roots(&mut store, "capture_intent"), 0);
        assert_eq!(capture(&mut store, &pending).unwrap_err().code(), "aborted");

        let done = request(repo.path(), "op-d", &base, &result);
        acknowledged(capture(&mut store, &done).unwrap());
        assert_eq!(
            abort(&mut store, &done.operation_id).unwrap_err().code(),
            "already_committed"
        );
        assert_eq!(
            abort(&mut store, &OperationId::parse("nope").unwrap())
                .unwrap_err()
                .code(),
            "unknown_operation"
        );
    }

    /// Repeated delivery: one operation sent concurrently yields one receipt.
    #[test]
    fn concurrent_deliveries_of_one_operation_produce_one_receipt() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        drop(open(host.path()));
        let receipts: Vec<CaptureReceipt> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let request = request(repo.path(), "op-1", &base, &result);
                    let host = host.path();
                    scope.spawn(move || {
                        let mut store = open(host);
                        acknowledged(capture(&mut store, &request).unwrap())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        assert!(receipts.windows(2).all(|pair| pair[0] == pair[1]));
        let mut store = open(host.path());
        assert_eq!(
            count(&mut store, "SELECT count(*) FROM capture_receipts"),
            1
        );
        assert_eq!(count(&mut store, "SELECT count(*) FROM outbox"), 1);
        assert_eq!(live_roots(&mut store, "contribution"), 1);
    }

    const CRASH_CHILD: &str = "AETHYME_CAPTURE_CRASH_CHILD";

    /// Run only as the child of the SIGKILL test: capture numbered
    /// operations and report each receipt after `capture` returned it.
    #[test]
    #[ignore = "child process of a_killed_capture_process_keeps_every_acknowledged_receipt"]
    fn crash_child_captures_until_killed() {
        let Some(spec) = std::env::var_os(CRASH_CHILD) else {
            return;
        };
        let spec = spec.to_string_lossy().into_owned();
        let mut parts = spec.split('\n');
        let (host, repo, base, result) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            CommitOid::parse(parts.next().unwrap()).unwrap(),
            CommitOid::parse(parts.next().unwrap()).unwrap(),
        );
        let mut store = open(Path::new(host));
        let stdout = std::io::stdout();
        for n in 1.. {
            let id = format!("op-{n}");
            let receipt = acknowledged(
                capture(&mut store, &request(Path::new(repo), &id, &base, &result)).unwrap(),
            );
            let mut out = stdout.lock();
            std::io::Write::write_all(
                &mut out,
                format!("ack {id} {}\n", receipt.receipt_record_id.as_str()).as_bytes(),
            )
            .unwrap();
            std::io::Write::flush(&mut out).unwrap();
        }
    }

    /// T08 with a real process death: every receipt the child reported
    /// survives with its bytes; anything in flight recovers and retries to
    /// exactly one receipt.
    #[test]
    fn a_killed_capture_process_keeps_every_acknowledged_receipt() {
        let (repo, base, result) = repo();
        let host = tempfile::tempdir().unwrap();
        let spec = format!(
            "{}\n{}\n{}\n{}",
            host.path().display(),
            repo.path().display(),
            base.as_str(),
            result.as_str()
        );
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "collaboration_capture::tests::crash_child_captures_until_killed",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CRASH_CHILD, spec)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut acked = Vec::new();
        for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines() {
            let line = line.unwrap();
            if let Some(rest) = line.split_once("ack ").map(|(_, rest)| rest.to_string()) {
                let (id, receipt) = rest.split_once(' ').unwrap();
                acked.push((id.to_string(), receipt.to_string()));
                if acked.len() >= 12 {
                    break;
                }
            }
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(acked.len() >= 12, "the child stopped early");

        let mut store = open(host.path());
        recover(&mut store).unwrap();
        for (id, receipt_id) in &acked {
            let found = receipt(&mut store, &OperationId::parse(id).unwrap())
                .unwrap()
                .unwrap_or_else(|| panic!("acknowledged {id} lost its receipt"));
            assert_eq!(found.receipt_record_id.as_str(), receipt_id);
            assert_reconstructs(&store, &found);
        }
        let in_flight: Vec<String> = store
            .connection()
            .prepare(
                "SELECT operation_id FROM capture_operations
                 WHERE state NOT IN ('committed', 'acknowledged')",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for id in in_flight {
            let request = request(repo.path(), &id, &base, &result);
            acknowledged(capture(&mut store, &request).unwrap());
        }
        let operations = count(&mut store, "SELECT count(*) FROM capture_operations");
        assert_eq!(
            count(&mut store, "SELECT count(*) FROM capture_receipts"),
            operations
        );
        assert_eq!(count(&mut store, "SELECT count(*) FROM outbox"), operations);
    }
}
