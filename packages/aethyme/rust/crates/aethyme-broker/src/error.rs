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

    #[error("no session with id {0}")]
    SessionNotFound(i64),

    #[error("no coordinated operation with id {0}")]
    CoordinatedOperationNotFound(i64),

    #[error("a session already exists for worktree {0}")]
    WorktreeAlreadyRegistered(String),

    #[error("invalid {field} value in broker db: {value:?}")]
    InvalidEnumValue { field: &'static str, value: String },
}
