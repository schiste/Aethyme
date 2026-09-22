//! Promotion is opt-in (#290 phase 2.2).
//!
//! A repository that ships through pull requests gets nothing from a second
//! copy of its work on an integration branch, while paying for it in drift
//! that leaks into every new session's base. Such a repository still wants
//! what `submit` is good at -- cross-session conflict detection before a pull
//! request exists, and affected gates rather than a full batch -- so the
//! verification and the promotion had to become separable.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{Broker, CachePolicy, MergeStatus, PromotionIntent};

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

fn fixture(promote_mode: Option<&str>) -> (tempfile::TempDir, Broker) {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    if let Some(mode) = promote_mode {
        std::fs::write(
            tmp.path().join(".aethyme/config.toml"),
            format!("[promote]\nmode = \"{mode}\"\n"),
        )
        .unwrap();
    }
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    let broker = Broker::open(tmp.path()).unwrap();
    (tmp, broker)
}

fn commit_work(broker: &mut Broker, task: &str) -> i64 {
    let session = broker.start_worktree(task, None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join(format!("{task}.txt")), "payload\n").unwrap();
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-qm", task]);
    session.id
}

/// The default is unchanged: a repository that says nothing still promotes.
#[test]
fn an_unconfigured_repository_still_promotes_automatically() {
    let (_tmp, mut broker) = fixture(None);
    let session = commit_work(&mut broker, "default");
    let outcome = broker.submit(session).unwrap();
    assert!(outcome.promoted, "{outcome:?}");
    assert!(outcome.promotion_suppressed.is_none());
    assert_eq!(outcome.entry.status, MergeStatus::Promoted);
}

/// `verify-only` verifies and moves nothing. The entry records the
/// verification, because the audit trail is the point of submitting at all.
#[test]
fn verify_only_repositories_verify_and_promote_nothing() {
    let (tmp, mut broker) = fixture(Some("verify-only"));
    let before = git(tmp.path(), &["rev-parse", "HEAD"]);
    let session = commit_work(&mut broker, "verified");

    let outcome = broker.submit(session).unwrap();
    assert!(!outcome.promoted, "{outcome:?}");
    assert_eq!(outcome.entry.status, MergeStatus::Verified);
    let reason = outcome
        .promotion_suppressed
        .expect("a verified entry that did not move must say why");
    assert!(reason.contains("promotion is off"), "{reason}");

    // The integration branch must not exist or must not have moved.
    let integration = git(tmp.path(), &["rev-parse", "--verify", "-q", "aethyme/integration"]);
    assert!(
        integration.is_empty() || integration == before,
        "integration moved to {integration} in a verify-only repository"
    );
}

/// The reason and the next action must agree. The first cut of `verify-only`
/// printed "promotion is off for this repository" and then, on the very next
/// line, "verified but not promoted (manual mode). Promote with `aethyme
/// broker promote`" -- naming the one command the mode exists to avoid,
/// directly under a sentence saying it would not happen.
#[test]
fn the_next_action_does_not_contradict_the_suppression_reason() {
    let (tmp, mut broker) = fixture(Some("verify-only"));
    let session = commit_work(&mut broker, "agreement");
    let outcome = broker.submit(session).unwrap();
    assert!(
        outcome
            .promotion_suppressed
            .as_deref()
            .is_some_and(|r| r.contains("promotion is off"))
    );

    let rendered = String::from_utf8_lossy(
        &std::process::Command::new(env!("CARGO_BIN_EXE_broker-cli-shim"))
            .args(["submit", "--session", &session.to_string()])
            .current_dir(tmp.path())
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned();
    assert!(
        !rendered.contains("manual mode"),
        "a verify-only repository must not be told it is in manual mode: {rendered}"
    );
    assert!(
        !rendered.contains("broker promote --entry"),
        "a verify-only repository must not be told to promote: {rendered}"
    );
}

/// `finish` must not demand a promotion the repository has opted out of, or
/// every session in it would be unfinishable.
#[test]
fn finish_does_not_demand_promotion_a_repository_opted_out_of() {
    let (_tmp, mut broker) = fixture(Some("verify-only"));
    let session = commit_work(&mut broker, "finishable");
    assert!(!broker.submit(session).unwrap().promoted);

    let report = broker.finish(session).unwrap();
    assert!(
        !report
            .warnings
            .iter()
            .any(|warning| warning.contains("verified but not promoted")),
        "{:?}",
        report.warnings
    );
}

/// `--verify-only` narrows one run of a promoting repository, and the reason
/// distinguishes the flag from the repository policy.
#[test]
fn the_flag_suppresses_promotion_for_one_run_without_changing_policy() {
    let (_tmp, mut broker) = fixture(None);
    let session = commit_work(&mut broker, "one-run");

    let outcome = broker
        .submit_with_intent(session, CachePolicy::Use, PromotionIntent::VerifyOnly)
        .unwrap();
    assert!(!outcome.promoted, "{outcome:?}");
    let reason = outcome.promotion_suppressed.unwrap();
    assert!(reason.contains("--verify-only"), "{reason}");

    // Policy is untouched: the next session promotes as before.
    let next = commit_work(&mut broker, "next-run");
    assert!(broker.submit(next).unwrap().promoted);
}
