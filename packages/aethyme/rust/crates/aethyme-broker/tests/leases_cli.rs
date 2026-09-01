use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{Broker, LeaseRoutingExportOptions};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) {
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
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join("src/owned.rs"), "owned\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    tmp
}

fn add_worktree(root: &Path, name: &str) -> std::path::PathBuf {
    let path = root.join(".aethyme/worktrees").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    git(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("agent/{name}"),
            path.to_str().unwrap(),
            "main",
        ],
    );
    path
}

#[test]
fn lease_plan_cli_renders_structured_and_text_results_without_mutation() {
    let tmp = fixture();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let planner_worktree = add_worktree(tmp.path(), "planner");
    let owner_worktree = add_worktree(tmp.path(), "owner");
    let planner = broker.adopt(&planner_worktree, None).unwrap();
    let owner = broker.adopt(&owner_worktree, None).unwrap();
    broker
        .store()
        .claim_lease(planner.id, "src/owned.rs", None)
        .unwrap();
    let directory = broker
        .store()
        .claim_lease(owner.id, "src/", Some(60_000))
        .unwrap();
    broker
        .store()
        .set_implicit_leases(owner.id, &["README.md".into()])
        .unwrap();

    let leases_before = serde_json::to_value(broker.store().active_leases().unwrap()).unwrap();
    let events_before =
        serde_json::to_value(broker.store().events_after(0, i64::MAX).unwrap()).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "schema = 1\n[leases.routing]\ndirty-secret = [\"private/\"]\n",
    )
    .unwrap();
    let planner_id = planner.id.to_string();

    let json_output = run(
        tmp.path(),
        &[
            "leases",
            "plan",
            "src/new.rs",
            "src/owned.rs",
            "README.md",
            "--session",
            &planner_id,
            "--json",
        ],
    );
    assert!(
        json_output.status.success(),
        "{}",
        String::from_utf8_lossy(&json_output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&json_output.stdout).unwrap();
    assert_eq!(report["session_id"], planner.id);
    assert_eq!(report["would_conflict"], true);
    assert_eq!(report["paths"][0]["path"], "README.md");
    assert_eq!(report["paths"][0]["conflicts"][0]["relation"], "exact");
    assert_eq!(report["paths"][0]["conflicts"][0]["session_id"], owner.id);
    assert_eq!(report["paths"][0]["conflicts"][0]["kind"], "implicit");
    assert_eq!(report["paths"][0]["conflicts"][0]["owner_status"], "active");
    assert_eq!(
        report["paths"][0]["conflicts"][0]["owner_worktree"],
        owner.worktree_path
    );
    assert!(
        report["paths"][0]["conflicts"][0]["safe_next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action
                .as_str()
                .unwrap()
                .contains("--reuse --path README.md"))
    );
    assert_eq!(report["paths"][1]["path"], "src/new.rs");
    assert_eq!(report["paths"][1]["conflicts"][0]["relation"], "directory");
    assert_eq!(
        report["paths"][1]["conflicts"][0]["expires_at"],
        directory.expires_at.unwrap()
    );
    assert_eq!(report["paths"][2]["path"], "src/owned.rs");
    assert_eq!(report["paths"][2]["owned"][0]["session_id"], planner.id);
    assert_eq!(report["paths"][2]["owned"][0]["kind"], "explicit");
    assert_eq!(report["paths"][2]["conflicts"][0]["session_id"], owner.id);

    let text_output = run(
        tmp.path(),
        &["leases", "plan", "src/owned.rs", "--session", &planner_id],
    );
    assert!(text_output.status.success());
    let text = String::from_utf8_lossy(&text_output.stdout);
    assert!(text.contains("src/owned.rs — would conflict"), "{text}");
    assert!(text.contains("owned"), "{text}");
    assert!(text.contains("conflict"), "{text}");
    assert!(text.contains("exact"), "{text}");
    assert!(text.contains("directory"), "{text}");
    assert!(text.contains("expires never"), "{text}");
    assert!(text.contains("owner active at"), "{text}");
    assert!(text.contains("next: aethyme broker adopt"), "{text}");
    assert!(
        text.contains(&format!("expires {}", directory.expires_at.unwrap())),
        "{text}"
    );

    assert_eq!(
        serde_json::to_value(broker.store().active_leases().unwrap()).unwrap(),
        leases_before
    );
    assert_eq!(
        serde_json::to_value(broker.store().events_after(0, i64::MAX).unwrap()).unwrap(),
        events_before
    );
    assert!(
        !tmp.path()
            .join(".aethyme/logs/command-metrics.jsonl")
            .exists(),
        "read-only lease planning must not write command telemetry"
    );
}

#[test]
fn closed_session_lease_claim_fails_without_persisting_a_phantom_lease() {
    let tmp = fixture();
    let worktree = add_worktree(tmp.path(), "closed-owner");
    let session_id = {
        let mut broker = Broker::open(tmp.path()).unwrap();
        let session = broker.adopt(&worktree, None).unwrap();
        broker.close(session.id).unwrap();
        session.id
    };

    let output = run(
        tmp.path(),
        &[
            "leases",
            "claim",
            "src/owned.rs",
            "--session",
            &session_id.to_string(),
            "--json",
        ],
    );
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("is closed and cannot authorize coordinated operations"),
        "{error}"
    );

    let mut broker = Broker::open(tmp.path()).unwrap();
    assert!(
        broker
            .store()
            .session_leases(session_id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn lease_plan_cli_requires_paths_and_rejects_ambiguous_spelling() {
    let tmp = fixture();
    Broker::open(tmp.path()).unwrap();

    let missing = run(tmp.path(), &["leases", "plan"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("plan requires at least one path"));

    let ambiguous = run(tmp.path(), &["leases", "plan", "src/../outside", "--json"]);
    assert!(!ambiguous.status.success());
    assert!(
        String::from_utf8_lossy(&ambiguous.stderr)
            .contains("`.` and `..` path segments are ambiguous")
    );
}

#[test]
fn lease_export_is_bounded_redacted_routed_and_read_only() {
    let tmp = fixture();
    git(
        tmp.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://build-user:credential@example.com/acme/product.git",
        ],
    );
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "schema = 1\n[leases.routing]\nbackend = [\"src/\"]\ndocumentation = [\"docs/\"]\n",
    )
    .unwrap();
    git(tmp.path(), &["add", "-f", ".aethyme/config.toml"]);
    git(tmp.path(), &["commit", "-qm", "routing config"]);

    let mut broker = Broker::open(tmp.path()).unwrap();
    let owner_worktree = add_worktree(tmp.path(), "secret-owner-path");
    let other_worktree = add_worktree(tmp.path(), "other");
    let owner = broker
        .adopt(&owner_worktree, Some("SECRET task text must not leak"))
        .unwrap();
    let other = broker.adopt(&other_worktree, None).unwrap();
    broker
        .store()
        .set_implicit_leases(owner.id, &["src/owned.rs".into()])
        .unwrap();
    broker.store().claim_lease(owner.id, "docs/", None).unwrap();
    broker
        .store()
        .claim_lease(owner.id, "expired.txt", Some(-1))
        .unwrap();
    broker
        .store()
        .claim_lease(owner.id, "released.txt", None)
        .unwrap();
    broker
        .store()
        .release_lease(owner.id, "released.txt")
        .unwrap();
    broker.store().claim_lease(other.id, "src/", None).unwrap();
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    let head = String::from_utf8(head.stdout).unwrap().trim().to_string();
    let entry = broker.store().submit(owner.id, &head, &head).unwrap();
    let leases_before =
        serde_json::to_value(broker.store().session_leases(owner.id).unwrap()).unwrap();
    let events_before =
        serde_json::to_value(broker.store().events_after(0, i64::MAX).unwrap()).unwrap();

    let owner_id = owner.id.to_string();
    let limited = run(
        tmp.path(),
        &[
            "leases",
            "export",
            "--session",
            &owner_id,
            "--limit",
            "2",
            "--json",
        ],
    );
    assert!(
        limited.status.success(),
        "{}",
        String::from_utf8_lossy(&limited.stderr)
    );
    let limited_json: serde_json::Value = serde_json::from_slice(&limited.stdout).unwrap();
    assert_eq!(limited_json["schema_version"], 1);
    assert_eq!(
        limited_json["repository"]["coordination_key"],
        "example.com/acme/product"
    );
    assert_eq!(limited_json["selector"]["session_id"], owner.id);
    assert_eq!(limited_json["total_matching"], 4);
    assert_eq!(limited_json["limit"], 2);
    assert_eq!(limited_json["truncated"], true);
    assert_eq!(limited_json["leases"].as_array().unwrap().len(), 2);

    let entry_id = entry.id.to_string();
    let complete = run(
        tmp.path(),
        &["leases", "export", "--entry", &entry_id, "--json"],
    );
    assert!(
        complete.status.success(),
        "{}",
        String::from_utf8_lossy(&complete.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&complete.stdout).unwrap();
    assert_eq!(report["selector"]["queue_entry_id"], entry.id);
    assert_eq!(
        report["routing_configuration"]["categories"],
        serde_json::json!(["backend", "documentation"])
    );
    let rows = report["leases"].as_array().unwrap();
    let docs = rows.iter().find(|row| row["path"] == "docs/").unwrap();
    assert_eq!(docs["path_kind"], "directory");
    assert_eq!(docs["lease_kind"], "explicit");
    assert_eq!(docs["state"], "active");
    assert_eq!(
        docs["routing_categories"],
        serde_json::json!(["documentation"])
    );
    let expired = rows
        .iter()
        .find(|row| row["path"] == "expired.txt")
        .unwrap();
    assert_eq!(expired["state"], "expired");
    let released = rows
        .iter()
        .find(|row| row["path"] == "released.txt")
        .unwrap();
    assert_eq!(released["state"], "released");
    let source = rows
        .iter()
        .find(|row| row["path"] == "src/owned.rs")
        .unwrap();
    assert_eq!(source["lease_kind"], "implicit");
    assert_eq!(source["conflict_state"], "overlapping");
    assert_eq!(
        source["conflicting_session_ids"],
        serde_json::json!([other.id])
    );
    assert_eq!(source["routing_categories"], serde_json::json!(["backend"]));

    let serialized = String::from_utf8(complete.stdout).unwrap();
    for forbidden in [
        "SECRET task text",
        "secret-owner-path",
        "build-user",
        "credential",
        "dirty-secret",
        owner.worktree_path.as_str(),
    ] {
        assert!(!serialized.contains(forbidden), "leaked {forbidden:?}");
    }
    assert_eq!(
        serde_json::to_value(broker.store().session_leases(owner.id).unwrap()).unwrap(),
        leases_before
    );
    assert_eq!(
        serde_json::to_value(broker.store().events_after(0, i64::MAX).unwrap()).unwrap(),
        events_before
    );
    assert!(
        !tmp.path()
            .join(".aethyme/logs/command-metrics.jsonl")
            .exists(),
        "export retries must not create telemetry or extend lease state"
    );
}

#[test]
fn lease_export_normalizes_one_remote_across_independent_clones() {
    fn exported_repository(remote: &str) -> serde_json::Value {
        let tmp = fixture();
        git(tmp.path(), &["remote", "add", "origin", remote]);
        let mut broker = Broker::open(tmp.path()).unwrap();
        let session = broker.adopt(tmp.path(), None).unwrap();
        broker
            .store()
            .claim_lease(session.id, "src/", None)
            .unwrap();
        let report = broker
            .export_lease_routing(
                LeaseRoutingExportOptions {
                    session_id: Some(session.id),
                    ..LeaseRoutingExportOptions::default()
                },
                1_700_000_000_000,
            )
            .unwrap();
        serde_json::to_value(report.repository).unwrap()
    }

    let https = exported_repository("https://example.com/acme/product.git");
    let scp = exported_repository("git@example.com:acme/product.git");
    assert_eq!(https["coordination_key"], scp["coordination_key"]);
    assert_eq!(https["display_slug"], scp["display_slug"]);
}

#[test]
fn lease_export_requires_a_selector_and_valid_bounds() {
    let tmp = fixture();
    git(
        tmp.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://example.com/acme/product.git",
        ],
    );
    Broker::open(tmp.path()).unwrap();

    let missing = run(tmp.path(), &["leases", "export", "--json"]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("requires --session"));

    let excessive = run(
        tmp.path(),
        &["leases", "export", "--session", "1", "--limit", "1001"],
    );
    assert!(!excessive.status.success());
    assert!(String::from_utf8_lossy(&excessive.stderr).contains("between 1 and 1000"));
}
