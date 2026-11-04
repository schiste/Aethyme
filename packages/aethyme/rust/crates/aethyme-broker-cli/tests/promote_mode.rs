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

mod common;

use aethyme_broker::{Broker, CachePolicy, MergeStatus, PromotionIntent};
use common::{FixtureRepository, open_fixture_repository_broker};

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

fn fixture(promote_mode: Option<&str>) -> (FixtureRepository, common::FixtureBroker) {
    let fixture = FixtureRepository::new();
    git(fixture.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(fixture.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(fixture.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::create_dir_all(fixture.path().join(".aethyme")).unwrap();
    if let Some(mode) = promote_mode {
        std::fs::write(
            fixture.path().join(".aethyme/config.toml"),
            format!("[promote]\nmode = \"{mode}\"\n"),
        )
        .unwrap();
    }
    git(fixture.path(), &["add", "-A"]);
    git(fixture.path(), &["commit", "-qm", "init"]);
    let broker = open_fixture_repository_broker(&fixture).unwrap();
    (fixture, broker)
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
    let (_fixture, mut broker) = fixture(None);
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
    let (fixture, mut broker) = fixture(Some("verify-only"));
    let before = git(fixture.path(), &["rev-parse", "HEAD"]);
    let session = commit_work(&mut broker, "verified");

    let outcome = broker.submit(session).unwrap();
    assert!(!outcome.promoted, "{outcome:?}");
    assert_eq!(outcome.entry.status, MergeStatus::Verified);
    let reason = outcome
        .promotion_suppressed
        .expect("a verified entry that did not move must say why");
    assert!(reason.contains("promotion is off"), "{reason}");

    // The integration branch must not exist or must not have moved.
    let integration = git(
        fixture.path(),
        &["rev-parse", "--verify", "-q", "aethyme/integration"],
    );
    assert!(
        integration.is_empty() || integration == before,
        "integration moved to {integration} in a verify-only repository"
    );
}

/// A misspelled critical mode stays visible and can never re-enable auto-promotion.
#[test]
fn unknown_mode_warns_in_status_and_submit_and_falls_back_to_verify_only() {
    let (tmp, mut broker) = fixture(Some("verify-onyl"));
    let before = git(tmp.path(), &["rev-parse", "HEAD"]);
    let session = commit_work(&mut broker, "unknown-mode");

    let status = broker.status_brief(0).unwrap();
    let warning = status
        .advice
        .iter()
        .find(|advice| advice.id == "promote.mode-invalid")
        .expect("status surfaces an invalid promotion mode");
    assert_eq!(warning.severity.as_str(), "warning");
    assert!(
        warning.summary.contains("verify-onyl"),
        "{}",
        warning.summary
    );
    assert!(
        warning.summary.contains("verify-only"),
        "{}",
        warning.summary
    );

    let session_id = session.to_string();
    let mut command = common::broker_cli(
        env!("CARGO_BIN_EXE_broker-cli-shim"),
        &["submit", "--session", &session_id, "--json"],
    );
    broker
        .configure_storage_context_for_child(&mut command)
        .unwrap();
    let output = command.current_dir(tmp.path()).output().unwrap();
    assert!(
        output.status.success(),
        "submit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let outcome: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(outcome["promoted"], false);
    assert!(
        outcome["promotion_suppressed"]
            .as_str()
            .is_some_and(|warning| warning.contains("verify-onyl"))
    );
    let integration = git(
        tmp.path(),
        &["rev-parse", "--verify", "-q", "aethyme/integration"],
    );
    assert!(
        integration.is_empty() || integration == before,
        "unknown mode promoted work to {integration}"
    );
}

/// The reason and the next action must agree. The first cut of `verify-only`
/// printed "promotion is off for this repository" and then, on the very next
/// line, "verified but not promoted (manual mode). Promote with `aethyme
/// broker promote`" -- naming the one command the mode exists to avoid,
/// directly under a sentence saying it would not happen.
#[test]
fn the_next_action_does_not_contradict_the_suppression_reason() {
    let (fixture, mut broker) = fixture(Some("verify-only"));
    let session = commit_work(&mut broker, "agreement");
    let outcome = broker.submit(session).unwrap();
    assert!(
        outcome
            .promotion_suppressed
            .as_deref()
            .is_some_and(|r| r.contains("promotion is off"))
    );

    let mut command = common::broker_cli(
        env!("CARGO_BIN_EXE_broker-cli-shim"),
        &["submit", "--session", &session.to_string()],
    );
    command.current_dir(fixture.path());
    broker
        .configure_storage_context_for_child(&mut command)
        .unwrap();
    let rendered = String::from_utf8_lossy(&command.output().unwrap().stdout).into_owned();
    assert!(
        !rendered.contains("manual mode"),
        "a verify-only repository must not be told it is in manual mode: {rendered}"
    );
    assert!(
        !rendered.contains("promote --entry"),
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

/// `finish` refuses correctly when work exists nowhere else, but the remedy it
/// names has to be reachable. In a repository that does not promote, `submit`
/// has already run and can never satisfy the check -- naming it sends the
/// operator round a loop with no exit, which is the engine-pin shape from #254.
#[test]
fn finish_names_a_reachable_remedy_where_promotion_is_off() {
    let (_tmp, mut broker) = fixture(Some("verify-only"));
    let session = commit_work(&mut broker, "unlanded");
    assert!(!broker.submit(session).unwrap().promoted);

    let report = broker.finish(session).unwrap();

    // The guard still holds: this work is on no delivery target.
    let warning = report
        .warnings
        .iter()
        .find(|w| w.contains("not represented on any delivery target"))
        .unwrap_or_else(|| panic!("expected the representation refusal: {:?}", report.warnings));
    assert!(
        warning.contains("does not promote"),
        "the refusal must say why submitting again cannot help: {warning}"
    );

    let commands = report.next_commands.join("\n");
    assert!(
        !commands.contains("broker submit"),
        "submit cannot satisfy this check here and must not be suggested: {commands}"
    );
    assert!(
        commands.contains("representation scan"),
        "the reachable path starts with a scan: {commands}"
    );
    // `record` is deliberately absent: it needs a digest the scan produces, so
    // naming it here could only be a placeholder.
    assert!(
        !commands.contains("<sha256>"),
        "a suggested command must be runnable as printed: {commands}"
    );
}

/// A promoting repository is unchanged: submit is the right next step there.
#[test]
fn finish_still_tells_a_promoting_repository_to_submit() {
    let (_tmp, mut broker) = fixture(Some("manual"));
    let session = commit_work(&mut broker, "held");
    assert!(!broker.submit(session).unwrap().promoted);

    let report = broker.finish(session).unwrap();
    let commands = report.next_commands.join("\n");
    assert!(
        commands.contains("broker promote") || commands.contains("broker submit"),
        "a promoting repository keeps its promotion remedy: {commands} / {:?}",
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

#[test]
fn configuration_reads_skip_rediscovery_and_recheck_the_current_default_ref() {
    const PROBE_ROOT: &str = "AETHYME_CONFIG_READ_PROBE_ROOT";
    const NEXT_COMMIT: &str = "AETHYME_CONFIG_READ_NEXT_COMMIT";
    const TEST: &str = "configuration_reads_skip_rediscovery_and_recheck_the_current_default_ref";
    fn isolated_git(root: &Path, home: &Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let output = command
            .args(args)
            .current_dir(root)
            .env("HOME", home)
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Config read fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Config read fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
    if let Some(root) = std::env::var_os(PROBE_ROOT) {
        let root = std::path::PathBuf::from(root);
        let home = root.parent().unwrap().join("home");
        assert!(matches!(
            aethyme_broker::PromoteConfig::load(&root).mode,
            aethyme_broker::PromoteMode::Manual
        ));
        let next = std::env::var(NEXT_COMMIT).unwrap();
        isolated_git(
            &root,
            &home,
            &["update-ref", "refs/remotes/origin/main", &next],
        );
        assert!(matches!(
            aethyme_broker::PromoteConfig::load(&root).mode,
            aethyme_broker::PromoteMode::VerifyOnly
        ));
        // Dirty local configuration cannot override the newly selected
        // committed default policy. No cached text or ref is authority.
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[promote]\nmode = \"auto\"\n",
        )
        .unwrap();
        assert!(matches!(
            aethyme_broker::PromoteConfig::load(&root).mode,
            aethyme_broker::PromoteMode::VerifyOnly
        ));
        return;
    }
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("repo");
    let home = fixture.path().join("home");
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    isolated_git(&root, &home, &["init", "-q", "-b", "main"]);
    let config = root.join(".aethyme/config.toml");
    std::fs::write(&config, "[promote]\nmode = \"manual\"\n").unwrap();
    isolated_git(&root, &home, &["add", ".aethyme/config.toml"]);
    isolated_git(
        &root,
        &home,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "manual policy",
        ],
    );
    let first = isolated_git(&root, &home, &["rev-parse", "HEAD"]);
    std::fs::write(&config, "[promote]\nmode = \"verify-only\"\n").unwrap();
    isolated_git(&root, &home, &["add", ".aethyme/config.toml"]);
    isolated_git(
        &root,
        &home,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "verify policy",
        ],
    );
    let next = isolated_git(&root, &home, &["rev-parse", "HEAD"]);
    isolated_git(
        &root,
        &home,
        &["update-ref", "refs/remotes/origin/main", &first],
    );
    isolated_git(
        &root,
        &home,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let trace = fixture.path().join("config-read-trace.jsonl");
    let mut child = Command::new(std::env::current_exe().unwrap());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            child.env_remove(key);
        }
    }
    let output = child
        .args(["--exact", TEST, "--nocapture"])
        .env(PROBE_ROOT, &root)
        .env(NEXT_COMMIT, next)
        .env("HOME", &home)
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TRACE2_EVENT", &trace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let events = std::fs::read_to_string(trace).unwrap();
    let starts: Vec<serde_json::Value> = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["event"] == "start")
        .collect();
    let tree_reads = starts
        .iter()
        .filter(|event| {
            event["argv"][1] == "ls-tree"
                && event["argv"].as_array().is_some_and(|argv| {
                    argv.iter()
                        .any(|arg| arg.as_str().is_some_and(|arg| arg.ends_with("config.toml")))
                })
        })
        .count();
    let blob_reads = starts
        .iter()
        .filter(|event| event["argv"][1] == "cat-file" && event["argv"][2] == "blob")
        .count();
    let default_ref_reads = starts
        .iter()
        .filter(|event| {
            event["argv"][1] == "symbolic-ref"
                && event["argv"][2] == "--quiet"
                && event["argv"][3] == "refs/remotes/origin/HEAD"
        })
        .count();
    assert_eq!(
        (tree_reads, blob_reads, default_ref_reads),
        (3, 3, 3),
        "positive witness: each policy load resolves the current default and reads committed content"
    );
    let discovery = starts
        .iter()
        .filter(|event| event["argv"][1] == "rev-parse" && event["argv"][2] == "--show-toplevel")
        .count();
    assert_eq!(
        discovery, 0,
        "known-root policy reads must not rediscover the checkout"
    );
}
