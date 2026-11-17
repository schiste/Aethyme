//! Whether one transcript may be archived: a pure decision over facts the
//! caller gathered, so every rule is unit-tested without a file system.
//!
//! A transcript is archivable only when all of these hold:
//! - it has been inactive for more than `archive_after_inactive_days`: its
//!   last activity is the newest of its modification time, the timestamp of
//!   its last record (read from the file's tail) and the last activity of
//!   every broker session linked to it;
//! - when broker sessions are linked to it, every one is closed or cleaned
//!   and its checkout directory no longer exists;
//! - no process has it open (checked last, because it is the expensive
//!   fact, and checked again by the archiver right before it acts).
//!
//! An unlinked transcript needs only the inactivity and not-open conditions.
//! Anything unreadable or ambiguous is a skip with a named reason.

use serde::Serialize;

pub(crate) const DAY_MS: i64 = 86_400_000;

/// Why a transcript was left in place. Every skip is counted by reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The modification time could not be read.
    MtimeUnreadable,
    /// Modified within the configured number of inactive days.
    TooRecent,
    /// Its last record is timestamped within the configured inactive days
    /// (the modification time alone was old enough).
    RecentRecord,
    /// A linked broker session was active within the configured inactive
    /// days.
    RecentSessionActivity,
    /// A linked broker database or session row could not be read.
    LinkageUnreadable,
    /// A linked broker session is still open.
    SessionOpen,
    /// A linked session's checkout directory still exists.
    CheckoutPresent,
    /// Whether a linked session's checkout exists could not be decided.
    CheckoutUnknown,
    /// A process has the transcript open.
    OpenByProcess,
    /// The open-file listing was incomplete, so "not open" is unproven.
    OpenCheckUnavailable,
    /// The transcript changed while it was being compressed.
    ModifiedDuringCompression,
    /// The compressed copy did not decompress to the source's bytes.
    VerifyFailed,
    /// Something else already sits at the archive destination.
    DestinationConflict,
    /// A file-system operation failed.
    Io,
    /// The path is not valid UTF-8, so the index cannot record it.
    UnsupportedPath,
    /// The provider root failed validation (missing, a symbolic link, not
    /// owned by the current user), or the path follows a link or crosses a
    /// mount point beneath it.
    UnsafePath,
    /// A test stopped the archiver at a crash point.
    Interrupted,
    /// A Claude Code process may be the transcript's live session (its
    /// working directory maps to the project, or it names the session).
    AgentRunning,
    /// The process table could not be read, so a live Claude Code session
    /// cannot be ruled out.
    ProcessCheckUnavailable,
    /// The archive volume lacks room for the compressed copy.
    InsufficientSpace,
    /// A destination conflict was recorded earlier; retried after a backoff.
    DestinationConflictBackoff,
    /// The copy could not be flushed to stable storage: the source was kept.
    SyncFailed,
}

impl SkipReason {
    pub fn label(self) -> &'static str {
        match self {
            SkipReason::MtimeUnreadable => "mtime_unreadable",
            SkipReason::TooRecent => "too_recent",
            SkipReason::RecentRecord => "recent_record",
            SkipReason::RecentSessionActivity => "recent_session_activity",
            SkipReason::LinkageUnreadable => "linkage_unreadable",
            SkipReason::SessionOpen => "session_open",
            SkipReason::CheckoutPresent => "checkout_present",
            SkipReason::CheckoutUnknown => "checkout_unknown",
            SkipReason::OpenByProcess => "open_by_process",
            SkipReason::OpenCheckUnavailable => "open_check_unavailable",
            SkipReason::ModifiedDuringCompression => "modified_during_compression",
            SkipReason::VerifyFailed => "verify_failed",
            SkipReason::DestinationConflict => "destination_conflict",
            SkipReason::Io => "io",
            SkipReason::UnsupportedPath => "unsupported_path",
            SkipReason::UnsafePath => "unsafe_path",
            SkipReason::Interrupted => "interrupted",
            SkipReason::AgentRunning => "agent_running",
            SkipReason::ProcessCheckUnavailable => "process_check_unavailable",
            SkipReason::InsufficientSpace => "insufficient_space",
            SkipReason::DestinationConflictBackoff => "destination_conflict_backoff",
            SkipReason::SyncFailed => "sync_failed",
        }
    }
}

/// Whether a linked session's checkout directory exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckoutState {
    Absent,
    Present,
    Unknown,
}

/// One broker session linked to a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkedSession {
    /// The session row's `cleanup_state` (`open`, `closed`, `cleaned`).
    pub cleanup_state: Option<String>,
    pub checkout: CheckoutState,
    /// The session row's `last_activity_at`, in milliseconds.
    pub last_activity_ms: Option<i64>,
}

/// What the broker databases say about a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Linkage {
    Unlinked,
    Linked(Vec<LinkedSession>),
    Unreadable,
}

/// Whether any process has the transcript open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenState {
    Closed,
    Open,
    Unknown,
}

/// The facts the static rules need.
#[derive(Debug, Clone)]
pub(crate) struct Facts<'a> {
    pub now_ms: i64,
    pub modified_ms: Option<i64>,
    /// The timestamp of the transcript's last record, when its tail named
    /// one.
    pub record_ms: Option<i64>,
    pub inactive_days: u32,
    pub linkage: &'a Linkage,
}

/// Whether `activity_ms` is more than `inactive_days` before `now_ms`. A
/// time in the future counts as recent.
pub(crate) fn inactive_long_enough(now_ms: i64, activity_ms: i64, inactive_days: u32) -> bool {
    let threshold = i64::from(inactive_days).saturating_mul(DAY_MS);
    now_ms.saturating_sub(activity_ms) > threshold
}

/// The modification-time rule alone, used to drop recent files before any
/// tail read or linkage work: the last activity is never older than the
/// modification time.
pub(crate) fn judge_age(
    now_ms: i64,
    modified_ms: Option<i64>,
    inactive_days: u32,
) -> Result<(), SkipReason> {
    let Some(modified) = modified_ms else {
        return Err(SkipReason::MtimeUnreadable);
    };
    if !inactive_long_enough(now_ms, modified, inactive_days) {
        return Err(SkipReason::TooRecent);
    }
    Ok(())
}

/// The last-record rule: a record timestamped within the inactive days
/// keeps the transcript.
pub(crate) fn judge_record(
    now_ms: i64,
    record_ms: Option<i64>,
    inactive_days: u32,
) -> Result<(), SkipReason> {
    match record_ms {
        Some(record) if !inactive_long_enough(now_ms, record, inactive_days) => {
            Err(SkipReason::RecentRecord)
        }
        _ => Ok(()),
    }
}

/// Every rule except "not open".
pub(crate) fn judge_static(facts: &Facts<'_>) -> Result<(), SkipReason> {
    judge_age(facts.now_ms, facts.modified_ms, facts.inactive_days)?;
    judge_record(facts.now_ms, facts.record_ms, facts.inactive_days)?;
    match facts.linkage {
        Linkage::Unlinked => Ok(()),
        Linkage::Unreadable => Err(SkipReason::LinkageUnreadable),
        Linkage::Linked(sessions) => {
            let all_closed = sessions.iter().all(|session| {
                matches!(session.cleanup_state.as_deref(), Some("closed" | "cleaned"))
            });
            if !all_closed {
                return Err(SkipReason::SessionOpen);
            }
            for session in sessions {
                match session.checkout {
                    CheckoutState::Absent => {}
                    CheckoutState::Present => return Err(SkipReason::CheckoutPresent),
                    CheckoutState::Unknown => return Err(SkipReason::CheckoutUnknown),
                }
            }
            let recent_session = sessions.iter().any(|session| {
                session.last_activity_ms.is_some_and(|activity| {
                    !inactive_long_enough(facts.now_ms, activity, facts.inactive_days)
                })
            });
            if recent_session {
                return Err(SkipReason::RecentSessionActivity);
            }
            Ok(())
        }
    }
}

/// The "not open" rule.
pub(crate) fn judge_open(open: OpenState) -> Result<(), SkipReason> {
    match open {
        OpenState::Closed => Ok(()),
        OpenState::Open => Err(SkipReason::OpenByProcess),
        OpenState::Unknown => Err(SkipReason::OpenCheckUnavailable),
    }
}

/// The whole decision.
pub(crate) fn judge(facts: &Facts<'_>, open: OpenState) -> Result<(), SkipReason> {
    judge_static(facts)?;
    judge_open(open)
}
