//! A confirmed journal whose reviewed files changed must finish, not wedge.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{Broker, BrokerOpError, GcFileAction, GcFileCandidate, RetentionPolicy};

use super::{
    GcJournal, now_ms, resume_backoff, retained_metric_bytes, sha256, utc_day_floor_ms,
    write_journal,
};

const METRICS: &str = ".aethyme/logs/command-metrics.jsonl";
const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Repository {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    _storage: crate::test_storage::TestStorage,
    broker: Broker,
}

impl Repository {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["add", "README.md"][..],
            &["commit", "-qm", "fixture"][..],
        ] {
            if args[0] == "add" {
                std::fs::write(root.join("README.md"), "fixture\n").unwrap();
            }
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        }
        std::fs::create_dir_all(root.join(".aethyme/logs/gates")).unwrap();
        std::fs::write(
            root.join(".aethyme/broker.toml"),
            "[retention]\nartifact_sweep_budget_ms = 0\nstartup_budget_ms = 5000\n",
        )
        .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(&root).unwrap();
        Self {
            _temporary: temporary,
            root,
            _storage: storage,
            broker,
        }
    }

    fn journal_path(&self) -> PathBuf {
        self.root.join(".aethyme/gc-journal.json")
    }

    /// Publish a schema 4 journal shaped exactly as binaries before the
    /// metrics trim rule wrote it: the rewrite carries whole-file hashes.
    fn publish_journal(&self, files: Vec<GcFileCandidate>) -> GcJournal {
        let evaluated_at = now_ms();
        let journal: GcJournal = serde_json::from_value(serde_json::json!({
            "schema_version": super::GC_PLAN_SCHEMA_VERSION,
            "digest": DIGEST,
            "evaluated_at": evaluated_at,
            "age_anchor_ms": utc_day_floor_ms(evaluated_at),
            "policy": RetentionPolicy::default(),
            "remaining_rows": [],
            "remaining_files": files,
            "remaining_worktrees": [],
            "rows_removed": 0,
            "files_completed": [],
            "sessions_cleaned": [],
            "reclaimed_bytes": 0
        }))
        .unwrap();
        write_journal(&self.journal_path(), &journal).unwrap();
        journal
    }
}

fn metric_row(ts: i64, command: &str) -> String {
    format!("{{\"ts\":{ts},\"command\":\"{command}\"}}\n")
}

fn append(path: &Path, text: &str) {
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

/// The rewrite candidate a plan made from `reviewed`, by the old rule.
fn reviewed_metrics_candidate(reviewed: &str, cutoff: i64) -> GcFileCandidate {
    let after = retained_metric_bytes(reviewed.as_bytes(), cutoff);
    GcFileCandidate {
        path: METRICS.into(),
        action: GcFileAction::Rewrite,
        before_sha256: sha256(reviewed.as_bytes()),
        after_sha256: Some(sha256(&after)),
        bytes_before: reviewed.len() as u64,
        bytes_after: after.len() as u64,
        source_row_ids: Vec::new(),
    }
}

fn metrics_cutoff() -> i64 {
    let days = RetentionPolicy::default().command_metrics_days;
    super::cutoff(utc_day_floor_ms(now_ms()), days)
}

#[test]
fn a_pre_change_journal_with_a_stale_metrics_hash_resumes_and_keeps_new_rows() {
    let mut repository = Repository::new();
    let metrics = repository.root.join(METRICS);
    let now = now_ms();
    let reviewed = format!(
        "{}not-json\n{}",
        metric_row(1, "expired"),
        metric_row(now, "reviewed")
    );
    std::fs::write(&metrics, &reviewed).unwrap();
    let candidate = reviewed_metrics_candidate(&reviewed, metrics_cutoff());
    repository.publish_journal(vec![candidate]);
    // Rows other commands appended after the review: new, expired, and one
    // without a timestamp. The journal's hashes no longer match the file.
    let appended = format!(
        "{}{}{{\"command\":\"no-ts\"}}\n",
        metric_row(now + 1, "appended"),
        metric_row(2, "appended-expired")
    );
    append(&metrics, &appended);

    let report = repository
        .broker
        .resume_confirmed_gc_journal_only()
        .unwrap()
        .expect("the journal is resumed");

    assert!(report.complete, "{report:?}");
    assert_eq!(report.files_completed, vec![METRICS.to_string()]);
    assert!(report.files_skipped.is_empty(), "{report:?}");
    assert_eq!(
        std::fs::read_to_string(&metrics).unwrap(),
        format!(
            "not-json\n{}{}{{\"command\":\"no-ts\"}}\n",
            metric_row(now, "reviewed"),
            metric_row(now + 1, "appended")
        ),
        "only rows older than the reviewed cutoff are dropped"
    );
    assert!(!repository.journal_path().exists());
}

#[test]
fn a_drifted_gate_log_is_retained_and_the_journal_still_completes() {
    let mut repository = Repository::new();
    let drifted = repository.root.join(".aethyme/logs/gates/drifted.log");
    let unchanged = repository.root.join(".aethyme/logs/gates/unchanged.log");
    std::fs::write(&drifted, "rewritten after review\n").unwrap();
    std::fs::write(&unchanged, "reviewed\n").unwrap();
    let delete = |path: &str, reviewed: &str| GcFileCandidate {
        path: path.into(),
        action: GcFileAction::Delete,
        before_sha256: sha256(reviewed.as_bytes()),
        after_sha256: None,
        bytes_before: reviewed.len() as u64,
        bytes_after: 0,
        source_row_ids: vec![1],
    };
    let metrics = repository.root.join(METRICS);
    let reviewed = metric_row(1, "expired");
    std::fs::write(&metrics, &reviewed).unwrap();
    repository.publish_journal(vec![
        delete(".aethyme/logs/gates/drifted.log", "reviewed\n"),
        delete(".aethyme/logs/gates/unchanged.log", "reviewed\n"),
        reviewed_metrics_candidate(&reviewed, metrics_cutoff()),
    ]);

    let report = repository
        .broker
        .resume_confirmed_gc_journal_only()
        .unwrap()
        .expect("the journal is resumed");

    assert_eq!(
        std::fs::read_to_string(&drifted).unwrap(),
        "rewritten after review\n",
        "a file whose content changed is never deleted"
    );
    assert!(!unchanged.exists(), "the steps after it still ran");
    assert_eq!(std::fs::read_to_string(&metrics).unwrap(), "");
    assert!(report.complete, "{report:?}");
    assert_eq!(report.files_skipped.len(), 1, "{report:?}");
    let skipped = &report.files_skipped[0];
    assert_eq!(skipped.path, ".aethyme/logs/gates/drifted.log");
    assert!(
        skipped.reason.contains("retained, not deleted"),
        "{report:?}"
    );
    assert_eq!(
        report.files_completed,
        vec![
            ".aethyme/logs/gates/unchanged.log".to_string(),
            METRICS.to_string(),
        ]
    );
    assert!(!repository.journal_path().exists());
}

#[test]
fn a_failing_resume_backs_off_until_the_journal_changes_or_the_window_passes() {
    let mut repository = Repository::new();
    let mut journal = repository.publish_journal(Vec::new());
    // Schema 2 is never resumable: every attempt fails the same way.
    journal.schema_version = 2;
    write_journal(&repository.journal_path(), &journal).unwrap();
    let journal_bytes = std::fs::read(repository.journal_path()).unwrap();
    let start = 1_000_000;

    let first = repository
        .broker
        .apply_confirmed_gc_journal_at(journal.clone(), 5000, start)
        .unwrap_err();
    let BrokerOpError::GcJournalResumeFailed { digest, .. } = &first else {
        panic!("{first}");
    };
    assert_eq!(digest, DIGEST);
    let message = first.to_string();
    assert!(
        message.contains("schema 2 is no longer resumable"),
        "{message}"
    );
    assert!(
        message.contains(&format!("gc abandon --confirm {DIGEST}")),
        "{message}"
    );

    let second = repository
        .broker
        .apply_confirmed_gc_journal_at(journal.clone(), 5000, start + 60_000)
        .unwrap();
    assert!(second.is_none(), "the next open does not rerun the journal");
    assert_eq!(
        std::fs::read(repository.journal_path()).unwrap(),
        journal_bytes
    );

    let after_window = start + resume_backoff::RETRY_AFTER_MS;
    assert!(
        repository
            .broker
            .apply_confirmed_gc_journal_at(journal.clone(), 5000, after_window)
            .is_err(),
        "the backoff expires"
    );

    journal.evaluated_at += 1;
    write_journal(&repository.journal_path(), &journal).unwrap();
    assert!(
        repository
            .broker
            .apply_confirmed_gc_journal_at(journal.clone(), 5000, after_window + 1)
            .is_err(),
        "a changed journal is retried at once"
    );
}
