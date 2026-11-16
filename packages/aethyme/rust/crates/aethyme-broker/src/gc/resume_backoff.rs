//! Backoff for open-time resume of a confirmed GC journal that keeps failing.
//!
//! Every writable broker open resumes a pending journal. When that resume
//! fails for a reason the next open cannot fix, retrying it on every command
//! repeats the same warning forever. The failure is remembered in broker meta
//! with the journal bytes' hash, and open-time resume leaves that exact
//! journal alone until it changes or [`RETRY_AFTER_MS`] passes. An explicit
//! `gc apply` never consults this: the operator asked for the run.

use sha2::{Digest, Sha256};
use std::path::Path;

/// Broker meta key holding the last failed open-time resume.
pub(super) const FAILURE_KEY: &str = "gc_journal_resume_failure";
/// How long open-time resume leaves an unchanged failing journal alone.
pub(super) const RETRY_AFTER_MS: i64 = 15 * 60 * 1000;
pub(super) const RETRY_AFTER_MINUTES: i64 = RETRY_AFTER_MS / 60_000;

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct ResumeFailure {
    journal_sha256: String,
    failed_at_ms: i64,
}

/// The hash of the journal bytes on disk, or `None` when there are none.
pub(super) fn journal_fingerprint(journal_path: &Path) -> Option<String> {
    std::fs::read(journal_path)
        .ok()
        .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
}

/// The meta value recording a failed resume of the journal `fingerprint`.
pub(super) fn failure_record(fingerprint: &str, now_ms: i64) -> String {
    serde_json::to_string(&ResumeFailure {
        journal_sha256: fingerprint.to_owned(),
        failed_at_ms: now_ms,
    })
    .expect("a resume failure record serializes")
}

/// Whether open-time resume should skip the journal `fingerprint` at
/// `now_ms`, given the stored `record`. A record for other journal bytes, an
/// unreadable record, or one from the future never suppresses a resume.
pub(super) fn active(record: Option<&str>, fingerprint: &str, now_ms: i64) -> bool {
    record
        .and_then(|value| serde_json::from_str::<ResumeFailure>(value).ok())
        .is_some_and(|failure| {
            let age = now_ms.saturating_sub(failure.failed_at_ms);
            failure.journal_sha256 == fingerprint && (0..RETRY_AFTER_MS).contains(&age)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_same_journal_bytes_inside_the_window_are_skipped() {
        let record = failure_record("journal-a", 1_000);
        let window_end = 1_000 + RETRY_AFTER_MS;

        assert!(active(Some(&record), "journal-a", 1_000));
        assert!(active(Some(&record), "journal-a", window_end - 1));
        assert!(!active(Some(&record), "journal-a", window_end));
        assert!(
            !active(Some(&record), "journal-b", 1_001),
            "a changed journal retries"
        );
        assert!(
            !active(Some(&record), "journal-a", 999),
            "a clock step back retries"
        );
        assert!(!active(Some(""), "journal-a", 1_001));
        assert!(!active(None, "journal-a", 1_001));
    }
}
