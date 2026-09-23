//! Integration drift is reported and repaired by its actual shape (#290 phase 3).
//!
//! Two states were collapsed into one refusal: integration *diverged* from the
//! tracked upstream, which needs a reviewed reconciliation, and integration
//! merely *behind* it, which is a fast-forward that discards nothing. Treating
//! the second as the first meant every pull-request merge left the gap in
//! place, and every session started afterwards inherited it.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{AutomaticIntegrationCleanupState, Broker, StatusAdviceSeverity};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
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

/// A repository whose `main` tracks `origin/main`, with integration pinned at
/// the commit `main` started from, then `origin/main` advanced by `ahead`
/// commits. Integration is therefore a strict ancestor of upstream.
fn fixture(ahead: usize) -> (tempfile::TempDir, Broker) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "fixture\n").unwrap();
    std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "init"]);
    let base = git(root, &["rev-parse", "HEAD"]);

    git(
        root,
        &["update-ref", "refs/heads/aethyme/integration", &base],
    );
    // The upstream commits must NOT be on local `main`. `integration_head`
    // already fast-forwards integration onto the main checkout's HEAD when it
    // is an ancestor of it (#40), so advancing local main here would mask the
    // state under test -- integration behind the *tracked upstream* while the
    // local branch is behind it too, which is what a pull-request merge leaves
    // behind and what #290 phase 3.1 is about.
    git(root, &["switch", "-qc", "upstream-side"]);
    for n in 0..ahead {
        std::fs::write(root.join(format!("upstream{n}.txt")), "upstream\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", &format!("upstream {n}")]);
    }
    let head = git(root, &["rev-parse", "HEAD"]);
    git(root, &["switch", "-q", "main"]);
    git(root, &["update-ref", "refs/remotes/origin/main", &head]);
    git(root, &["config", "remote.origin.url", "."]);
    git(
        root,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    git(root, &["config", "branch.main.remote", "origin"]);
    git(root, &["config", "branch.main.merge", "refs/heads/main"]);

    let broker = Broker::open(root).unwrap();
    (tmp, broker)
}

/// Phase 3.1. Nothing was promoted, so the reconciliation plan is empty — the
/// case the previous guard required to be non-empty and therefore refused.
#[test]
fn automatic_cleanup_fast_forwards_when_integration_carries_nothing_of_its_own() {
    let (tmp, mut broker) = fixture(3);
    let upstream = git(tmp.path(), &["rev-parse", "refs/remotes/origin/main"]);

    let report = broker
        .auto_cleanup_landed_integration("refs/remotes/origin/main")
        .unwrap();

    assert_eq!(
        report.state,
        AutomaticIntegrationCleanupState::Cleaned,
        "{}",
        report.explanation
    );
    assert_eq!(report.new_integration, upstream);
    assert!(
        report.explanation.contains("fast-forwarded"),
        "{}",
        report.explanation
    );
    assert_eq!(
        git(tmp.path(), &["rev-parse", "refs/heads/aethyme/integration"]),
        upstream
    );
}

/// Phase 3.2. Behind-but-clean is a notice naming the repair, not a block:
/// blocking taught operators to wait for the block instead of keeping the ref
/// current, and to reach for a reviewed reconciliation when a fast-forward was
/// the whole answer.
#[test]
fn status_reports_a_clean_lag_as_a_notice_rather_than_a_block() {
    let (_tmp, broker) = fixture(2);
    let status = broker.status_snapshot(0).unwrap();

    let advice = status
        .advice
        .iter()
        .find(|entry| entry.id == "integration.fast-forward-available")
        .expect("a clean lag must be reported");

    assert_eq!(advice.severity, StatusAdviceSeverity::Notice);
    assert!(
        advice.summary.contains("fast-forward"),
        "{}",
        advice.summary
    );
    assert!(
        !status
            .advice
            .iter()
            .any(|entry| entry.severity == StatusAdviceSeverity::Blocked),
        "a clean lag must not block: {:?}",
        status.advice
    );
}

/// The guard that matters: integration holding work upstream lacks is
/// divergence, and must still refuse rather than fast-forward over it.
#[test]
fn divergence_is_still_refused() {
    let (tmp, mut broker) = fixture(2);
    let root = tmp.path();

    // Put a commit on integration that upstream does not have.
    let integration = git(root, &["rev-parse", "refs/heads/aethyme/integration"]);
    git(root, &["switch", "-qc", "tmp-integration", &integration]);
    std::fs::write(root.join("only-on-integration.txt"), "mine\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "unique to integration"]);
    let diverged = git(root, &["rev-parse", "HEAD"]);
    git(
        root,
        &["update-ref", "refs/heads/aethyme/integration", &diverged],
    );
    git(root, &["switch", "-q", "main"]);

    let report = broker
        .auto_cleanup_landed_integration("refs/remotes/origin/main")
        .unwrap();
    assert_eq!(
        report.state,
        AutomaticIntegrationCleanupState::Deferred,
        "{}",
        report.explanation
    );
    assert_eq!(
        git(root, &["rev-parse", "refs/heads/aethyme/integration"]),
        diverged,
        "a diverged integration ref must not move"
    );
}
