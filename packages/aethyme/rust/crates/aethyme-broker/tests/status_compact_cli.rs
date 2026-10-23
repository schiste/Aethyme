//! Compact status keeps repeated warnings visible without repeating each
//! session's evidence, while the established JSON contracts remain intact.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::OverlapSeverity;

mod common;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn clear_git_selectors(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    clear_git_selectors(&mut command);
    command
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "compact-status-test")
        .env("GIT_AUTHOR_EMAIL", "compact-status-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "compact-status-test")
        .env("GIT_COMMITTER_EMAIL", "compact-status-test@example.invalid");
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

fn add_worktree(repo: &Path, worktree_root: &Path, name: &str) -> PathBuf {
    let path = worktree_root.join(name);
    assert!(
        !path.starts_with(repo),
        "fixture worktrees stay outside the repository"
    );
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

    let worktree_root = fixture.worktree_root();
    for name in ["agent-a", "agent-b", "agent-c"] {
        let worktree = add_worktree(fixture.path(), &worktree_root, name);
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
        compact["summary"]["dirty_sessions"],
        serde_json::Value::Null
    );
    assert_eq!(
        compact["summary"]["promoted_conflict_count"],
        serde_json::Value::Null
    );
    assert_eq!(compact["summary"]["integration_relation"], "not_checked");
    assert_eq!(
        compact["summary"]["integration_ahead_main_commits"],
        serde_json::Value::Null
    );
    let unknown_summary_fields = compact["summary"]["unknown_fields"]
        .as_array()
        .expect("unknown summary fields are listed");
    for field in [
        "dirty_sessions",
        "promoted_conflict_count",
        "integration_ahead_main_commits",
        "integration_head",
        "baseline_ref",
        "baseline_head",
        "main_head",
        "integration_ahead_local_main_commits",
    ] {
        assert!(
            unknown_summary_fields.iter().any(|listed| listed == field),
            "null summary field {field} is declared unknown"
        );
    }
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
    assert_eq!(compact["waiters"]["details_omitted"], false);
    assert_eq!(compact["waiters"]["count"], 0);
    assert_eq!(compact["waiters"]["omitted_count"], 0);
    assert_eq!(compact["waiters"]["records"], serde_json::json!([]));
    assert_eq!(compact["coordinated_operations"]["count"], 0);
    assert_eq!(compact["lease_release_requests"]["count"], 0);

    let text = run_status(fixture.path(), &["status"]);
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Broker status (compact)"));
    assert!(text.contains("recorded lease-overlap observations"));
    assert!(text.contains("lease.overlap-conflict"));
    assert!(text.contains("count 3, 2 duplicates"));
    assert!(text.contains("Deferred checks (unknown, not clean)"));
    assert!(text.contains("aethyme broker status --json"));
    assert!(text.contains("Dirty sessions and promoted conflicts: not checked."));
    assert!(!text.contains("0 dirty"));
    if let Some(command) = compact["summary"]["commands"]
        .as_array()
        .and_then(|items| items.first())
        .and_then(serde_json::Value::as_str)
    {
        assert!(text.contains("Next:"));
        assert!(text.contains(command));
    }
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

#[test]
fn compact_status_retains_bounded_waiter_resources() {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let broker = common::open_fixture_repository_broker(&fixture).unwrap();
    drop(broker);
    let waits = fixture.path().join(".aethyme/run/waits");
    std::fs::create_dir_all(&waits).unwrap();
    for i in 0..12 {
        let record = aethyme_broker::WaitRecord {
            pid: i64::from(std::process::id()),
            session_id: Some(100 + i),
            kind: "lease".into(),
            resource: format!("resource-{i}"),
            holder: format!("session {}", 200 + i),
            started_at_ms: i,
        };
        std::fs::write(
            waits.join(format!("fixture-{i}.json")),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }
    let compact = json_status(fixture.path(), &["status", "--compact", "--json"]);
    assert_eq!(compact["waiters"]["count"], 12);
    assert_eq!(compact["waiters"]["omitted_count"], 2);
    assert_eq!(compact["waiters"]["details_omitted"], true);
    let rows = compact["waiters"]["records"].as_array().unwrap();
    assert_eq!(rows.len(), 10);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row["resource"], format!("resource-{i}"));
        assert_eq!(row["holder"], format!("session {}", 200 + i));
    }
    let text = run_status(fixture.path(), &["status"]);
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains("Waiters: 12 (2 additional rows omitted)."),
        "{text}"
    );
    assert!(text.contains("resource-0: session 200"), "{text}");
    assert!(!text.contains("resource-11"), "{text}");
}

#[test]
fn summary_defers_gate_history_queries_to_full_inspection() {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    let broker = common::open_fixture_repository_broker(&fixture).unwrap();
    // Make gate history unavailable independently of session/coordination
    // records. A full recovery inspection must report that fault; summary
    // explicitly defers this source and must not query it at all.
    rusqlite::Connection::open(aethyme_broker::broker_db_path(fixture.path()).unwrap())
        .unwrap()
        .execute_batch("ALTER TABLE gate_results RENAME TO unavailable_gate_history")
        .unwrap();
    let blockers = broker.blockers();
    assert!(
        blockers
            .unavailable
            .iter()
            .any(|source| source.source == "broker.db gate_results"),
        "full inspection must report unavailable gate history: {blockers:?}"
    );
    // Inspect through the already-open broker: a new CLI process correctly
    // refuses a database missing a required table during schema validation,
    // before it reaches the snapshot collector this regression exercises.
    let summary = serde_json::to_value(broker.status_brief_snapshot(0).unwrap()).unwrap();
    let advice = summary["advice"].as_array().unwrap();
    assert!(
        !advice.iter().any(|item| {
            item["id"] == "blockers.sources-unavailable"
                && item["evidence"].as_array().is_some_and(|evidence| {
                    evidence.iter().any(|entry| {
                        entry
                            .as_str()
                            .is_some_and(|text| text.starts_with("broker.db gate_results:"))
                    })
                })
        }),
        "routine status must not query deferred gate history: {summary}"
    );
}
