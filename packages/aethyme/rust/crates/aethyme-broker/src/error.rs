use std::path::PathBuf;

/// Errors surfaced by the broker store.
///
/// `SQLITE_BUSY` must never leak to callers as a raw code: the store opens
/// every connection with a busy timeout, and anything that still times out
/// surfaces as [`BrokerError::Sqlite`] with full context.
#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error("broker db i/o at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("broker db: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error(
        "broker db schema version {found} is newer than this binary supports ({supported}); \
         upgrade aethyme"
    )]
    SchemaTooNew { found: i64, supported: i64 },

    #[error(
        "broker db schema version {found} is outside this binary's non-mutating snapshot range {minimum}..={maximum}; use a compatible Aethyme binary for recovery"
    )]
    SnapshotSchemaMismatch {
        found: i64,
        minimum: i64,
        maximum: i64,
    },

    #[error("no session with id {0}")]
    SessionNotFound(i64),

    #[error(
        "session {session_id} checkpoint changed while recovery was being applied (expected {expected}, found {actual})"
    )]
    SessionCheckpointChanged {
        session_id: i64,
        expected: String,
        actual: String,
    },

    #[error("no coordinated operation with id {0}")]
    CoordinatedOperationNotFound(i64),

    #[error("no advisory with id {0}")]
    AdvisoryNotFound(i64),

    #[error("no session note with id {0}")]
    SessionNoteNotFound(i64),

    #[error("advisory identity {0:?} already exists with different immutable data")]
    AdvisoryIdentityConflict(String),

    #[error("no external coordination event with id {0}")]
    ExternalEventNotFound(i64),

    #[error(
        "external event {provider}/{event_id} was already received with a different normalized digest"
    )]
    ExternalEventIdentityConflict { provider: String, event_id: String },

    #[error("no review lifecycle for session {0}")]
    ReviewLifecycleNotFound(i64),

    #[error("invalid persisted review lifecycle state {0:?}")]
    InvalidReviewLifecycleState(String),

    #[error("review lifecycle identity already belongs to different immutable provenance")]
    ReviewLifecycleIdentityConflict,

    #[error("review lifecycle {id} changed concurrently: expected {expected}, found {actual}")]
    ReviewLifecycleStateChanged {
        id: i64,
        expected: String,
        actual: String,
    },

    #[error("entry path exposure for queue entry {0} already exists with different immutable data")]
    EntryExposureIdentityConflict(i64),

    #[error("invalid {field} JSON for advisory {id}: {source}")]
    InvalidAdvisoryJson {
        id: i64,
        field: &'static str,
        source: serde_json::Error,
    },

    #[error("invalid {field} JSON for entry path exposure {id}: {source}")]
    InvalidEntryExposureJson {
        id: i64,
        field: &'static str,
        source: serde_json::Error,
    },

    #[error("operation history --limit must be between 1 and {maximum}, got {limit}")]
    InvalidOperationHistoryLimit { limit: u32, maximum: u32 },

    #[error("merge queue history --limit must be between 1 and {maximum}, got {limit}")]
    InvalidMergeQueueHistoryLimit { limit: u32, maximum: u32 },

    #[error("a session already exists for worktree {0}")]
    WorktreeAlreadyRegistered(String),

    #[error(
        "planned lease {path:?} overlaps {blocker_kind} lease {blocker_path:?} held by session {blocker_session_id} ({blocker_status}) at {blocker_worktree:?}\nSafe next actions:\n  {remediation}"
    )]
    PlannedLeaseConflict {
        path: String,
        blocker_session_id: i64,
        blocker_path: String,
        blocker_kind: String,
        blocker_status: String,
        blocker_worktree: String,
        remediation: String,
    },

    #[error("invalid {field} value in broker db: {value:?}")]
    InvalidEnumValue { field: &'static str, value: String },
}
