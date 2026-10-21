//! Compact status keeps repeated warnings visible without repeating each
//! session's evidence, while the established JSON contracts remain intact.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{Broker, OverlapSeverity};

mod common;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn clear_git_selectors(command: &mut Command) {
    for selector in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(selector);
    }
}

fn git(repo: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "compact-status-test")
        .env("GIT_AUTHOR_EMAIL", "compact-status-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "compact-status-test")
        .env("GIT_COMMITTER_EMAIL", "compact-status-test@example.invalid");
    clear_git_selectors(&mut command);
    let output = command.output().expect("run fixture Git command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(repo: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join("src/shared.txt"), "base\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "init"]);
}

fn add_worktree(repo: &Path, name: &str) -> PathBuf {
    let path = repo.join(".aethyme/worktrees").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            name,
            path.to_str().unwrap(),
            "main",
        ],
    );
    path
}

fn run_status(repo: &Path, args: &[&str]) -> Output {
    let mut command = common::broker_cli_for_fixture(CLI, args, repo);
    clear_git_selectors(&mut command);
    command
        .current_dir(repo)
        .output()
        .expect("run broker status")
}

fn json_status(repo: &Path, args: &[&str]) -> serde_json::Value {
    let output = run_status(repo, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("status JSON")
}

#[test]
fn compact_status_groups_repeated_warnings_and_marks_omitted_details() {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let mut broker = common::open_fixture_repository_broker(&fixture).unwrap();

    for name in ["agent-a", "agent-b", "agent-c"] {
        let worktree = add_worktree(fixture.path(), name);
        broker.adopt(&worktree, Some(name)).unwrap();
        std::fs::write(worktree.join("src/shared.txt"), format!("{name}\n")).unwrap();
    }
    let overlaps = broker.refresh_leases().unwrap();
    assert!(overlaps.len() >= 3, "expected all three pairs to overlap");
    let high = broker
        .overlap_pairs_snapshot(&overlaps)
        .into_iter()
        .filter(|pair| pair.severity == OverlapSeverity::High)
        .count();
    assert_eq!(high, 3, "each pair has a same-line conflict");
    drop(broker);

    let compact = json_status(fixture.path(), &["status", "--compact", "--json"]);
    assert_eq!(compact["schema"], "aethyme.broker-status-compact/v1");
    assert_eq!(compact["data_freshness"]["leases_refreshed"], false);
    assert_eq!(
        compact["data_freshness"]["deferred_checks_are_unknown"],
        true
    );
    assert!(
        compact["data_freshness"]["deferred_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check == "git_refs")
    );

    let categories = compact["advice"]["categories"].as_array().unwrap();
    let overlap = categories
        .iter()
        .find(|category| category["id"] == "lease.overlap-conflict")
        .expect("overlap warning category retained");
    assert_eq!(overlap["severity"], "warning");
    assert_eq!(overlap["count"], 3);
    assert_eq!(overlap["duplicate_count"], 2);
    assert_eq!(overlap["summary_variants_omitted"], true);
    assert_eq!(overlap["details_omitted"], true);
    assert_eq!(
        overlap["commands"],
        serde_json::json!(["aethyme broker advanced leases --json"])
    );
    assert!(
        compact["omitted_details"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field == "advice.evidence")
    );
    assert_eq!(compact["waiters"]["details_omitted"], true);

    let text = run_status(fixture.path(), &["status"]);
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Broker status (compact)"));
    assert!(text.contains("lease.overlap-conflict"));
    assert!(text.contains("count 3, 2 duplicates"));
    assert!(text.contains("Deferred checks (unknown, not clean)"));
    assert!(text.contains("aethyme broker status --json"));
}

#[test]
fn compact_json_is_additive_and_legacy_json_shapes_are_unchanged() {
    let fixture = tempfile::tempdir().unwrap();
    init_repo(fixture.path());

    let full = json_status(fixture.path(), &["status", "--json"]);
    for key in [
        "agents",
        "blockers",
        "cleanup_retention",
        "deferred_checks",
        "leases_refreshed",
        "outstanding_advisories",
        "summary",
    ] {
        assert!(full.get(key).is_some(), "legacy full JSON still has {key}");
    }

    let summary = json_status(fixture.path(), &["status", "--summary", "--json"]);
    for key in [
        "advice",
        "deferred_checks",
        "leases_refreshed",
        "phase_timings_ms",
        "summary",
    ] {
        assert!(
            summary.get(key).is_some(),
            "legacy summary JSON still has {key}"
        );
    }
    assert!(summary.get("schema").is_none());

    let compact = json_status(fixture.path(), &["status", "--compact", "--json"]);
    assert_eq!(compact["schema"], "aethyme.broker-status-compact/v1");
    assert!(compact.get("agents").is_none());
    assert!(compact.get("blockers").is_none());

    for args in [
        ["status", "--compact", "--refresh"].as_slice(),
        ["status", "--compact", "--summary"].as_slice(),
    ] {
        let output = run_status(fixture.path(), args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("--compact"));
    }
}
