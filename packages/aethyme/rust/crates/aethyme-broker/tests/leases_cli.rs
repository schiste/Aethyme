use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::Broker;

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
