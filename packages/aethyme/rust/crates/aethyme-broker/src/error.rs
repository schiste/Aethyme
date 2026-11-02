use std::path::PathBuf;

/// Maximum GC row count validated and deleted in one SQLite transaction.
pub(crate) const GC_ROW_DELETE_BATCH_SIZE: usize = 128;

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

    /// A `.aethyme` configuration file could not be resolved or parsed.
    /// Boxed so the error enum stays small.
    #[error("{0}")]
    Config(Box<aethyme_config::ConfigSourceError>),

    // Recovery must never be "run whatever binary is newer" (#361): the
    // newer schema may have come from an unreviewed gate build, and the
    // only binaries that should open shared state are released ones.
    #[error(
        "broker db schema version {found} is newer than this binary supports ({supported}); \
         install a released aethyme that supports it (`aethyme update`), and do not build one \
         from an unreviewed worktree to recover shared state"
    )]
    SchemaTooNew { found: i64, supported: i64 },

    #[error("broker database compatibility metadata is missing or invalid: {0}")]
    InvalidSchemaMetadata(String),

    /// A gate child resolved, or was pointed at, broker state the gate
    /// protects. Refused before any connection is opened (#361).
    #[error("gate refused the broker database: {0}")]
    GateDatabaseRefused(String),

    #[error(
        "broker db schema version {found} is outside this binary's non-mutating snapshot range {minimum}..={maximum}; use a compatible Aethyme binary for recovery"
    )]
    SnapshotSchemaMismatch {
        found: i64,
        minimum: i64,
        maximum: i64,
    },

    #[error("repository worktree binding is missing or invalid: {0}")]
    InvalidRepositoryWorktreeBinding(String),

    #[error("repository worktree binding cannot transition from {current} to {requested}")]
    RepositoryWorktreeBindingTransition { current: String, requested: String },

    #[error("repository worktree binding confirmation does not match the reviewed plan digest")]
    RepositoryWorktreeBindingConfirmationMismatch,

    #[error("base failure incident refused: {0}")]
    InvalidBaseFailureIncident(String),

    #[error("no session with id {0}")]
    SessionNotFound(i64),

    #[error("session {0} is no longer open")]
    SessionNotOpen(i64),

    #[error("broker database is busy; session-retirement work was deferred")]
    SessionRetirementDatabaseBusy,

    #[error("session-retirement request state is invalid: {0}")]
    InvalidSessionRetirementState(String),

    #[error(
        "GC retention candidate {kind} row {id} changed or is now protected; the pending batch stopped"
    )]
    GcRowEligibilityChanged { kind: String, id: i64 },

    #[error("GC row batch has {requested} rows; the maximum is {max}")]
    GcRowBatchTooLarge { requested: usize, max: usize },

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

    #[error("advisory {0} is session-facing and cannot be suppressed")]
    AdvisorySuppressionNotAllowed(i64),

    #[error("no session note with id {0}")]
    SessionNoteNotFound(i64),

    #[error("no automation note with id {0}")]
    AutomationNoteNotFound(i64),

    #[error("pull request watch identity is already active for {repository} PR #{pr_number}")]
    PullRequestWatchIdentityConflict { repository: String, pr_number: i64 },

    #[error("pull request activity batch {0} was not found")]
    PullRequestActivityBatchNotFound(i64),

    #[error("pull request activity batch {0} was already acknowledged differently")]
    PullRequestActivityBatchAckConflict(i64),

    #[error("delivery outbox item {0} was not found")]
    DeliveryOutboxNotFound(i64),

    #[error(
        "delivery outbox claim changed for item {id}; expected worker {worker:?} generation {generation}"
    )]
    DeliveryClaimChanged {
        id: i64,
        worker: String,
        generation: i64,
    },

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

    #[error("invalid persisted representation discovery {0:?}")]
    InvalidRepresentationDiscovery(String),

    #[error("invalid persisted review lifecycle state {0:?}")]
    InvalidReviewLifecycleState(String),

    #[error("review lifecycle identity already belongs to different immutable provenance")]
    ReviewLifecycleIdentityConflict,

    #[error(
        "review lifecycle for {repository} PR #{pr_number} is already owned by session {session_id}; inspect it with `aethyme broker advanced review show --session {session_id}`, then reassign or abandon it explicitly"
    )]
    ReviewLifecyclePrOwned {
        repository: String,
        pr_number: i64,
        session_id: i64,
    },

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

    #[error("invalid inputs JSON for verification candidate queue entry {id}: {source}")]
    InvalidVerificationCandidateInputsJson { id: i64, source: serde_json::Error },

    #[error("operation history --limit must be between 1 and {maximum}, got {limit}")]
    InvalidOperationHistoryLimit { limit: u32, maximum: u32 },

    #[error("merge queue history --limit must be between 1 and {maximum}, got {limit}")]
    InvalidMergeQueueHistoryLimit { limit: u32, maximum: u32 },

    #[error("a session already exists for worktree {0}")]
    WorktreeAlreadyRegistered(String),

    #[error("invalid spawned worktree instance witness: {0}")]
    InvalidSpawnedWorktreeInstanceWitness(String),

    #[error("{0}")]
    PlannedLeaseConflict(Box<PlannedLeaseConflict>),

    #[error("invalid {field} value in broker db: {value:?}")]
    InvalidEnumValue { field: &'static str, value: String },
}

/// Detail for [`BrokerError::PlannedLeaseConflict`], boxed so the error enum
/// stays small on every `Result` path.
#[derive(Debug, thiserror::Error)]
#[error(
    "planned lease {path:?} overlaps {blocker_kind} lease {blocker_path:?} held by session {blocker_session_id} ({blocker_status}) at {blocker_worktree:?}\nSafe next actions:\n  {remediation}"
)]
pub struct PlannedLeaseConflict {
    pub path: String,
    pub blocker_session_id: i64,
    pub blocker_path: String,
    pub blocker_kind: String,
    pub blocker_status: String,
    pub blocker_worktree: String,
    pub remediation: String,
}

impl From<aethyme_config::ConfigSourceError> for BrokerError {
    fn from(error: aethyme_config::ConfigSourceError) -> Self {
        Self::Config(Box::new(error))
    }
}
