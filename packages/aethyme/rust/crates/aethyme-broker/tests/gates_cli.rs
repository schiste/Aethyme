use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use aethyme_broker::GitRepo;

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

fn git_output(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn run_with_stdin(repo: &Path, args: &[&str], input: &str) -> Output {
    let mut child = Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stdout(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(tmp.path().join("tracked.txt"), "first\n").unwrap();
    std::fs::write(
        tmp.path().join(".gitignore"),
        "gate-runs.txt\nsubmit-runs.txt\n.aethyme/broker.db*\n.aethyme/logs/\n.aethyme/run/\n.aethyme/worktrees/\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"provenance\"\ncommand = \"echo run >> gate-runs.txt\"\n",
    )
    .unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "fixture"]);
    tmp
}

fn tree_hash(repo: &Path) -> String {
    GitRepo::discover(repo)
        .unwrap()
        .working_tree_hash()
        .unwrap()
}

fn run_count(path: &Path) -> usize {
    std::fs::read_to_string(path).unwrap().lines().count()
}

#[test]
fn gate_cli_reports_tree_provenance_for_executed_and_cached_results() {
    let tmp = fixture();
    let first_tree = tree_hash(tmp.path());

    let executed_text = stdout(run(tmp.path(), &["gates", "run", "--all"]));
    assert!(executed_text.contains(&format!("(tree {})", &first_tree[..12])));
    assert!(!executed_text.contains("(cached)"));
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 1);

    let cached_json = stdout(run(tmp.path(), &["gates", "run", "--all", "--json"]));
    let cached: serde_json::Value = serde_json::from_str(&cached_json).unwrap();
    assert_eq!(cached[0]["cached"], true);
    assert_eq!(cached[0]["tree_hash"], first_tree);
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 1);

    std::fs::write(tmp.path().join("tracked.txt"), "second\n").unwrap();
    let second_tree = tree_hash(tmp.path());
    assert_ne!(second_tree, first_tree);

    let executed_json = stdout(run(tmp.path(), &["gates", "run", "--all", "--json"]));
    let executed: serde_json::Value = serde_json::from_str(&executed_json).unwrap();
    assert_eq!(executed[0]["cached"], false);
    assert_eq!(executed[0]["tree_hash"], second_tree);
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 2);

    let cached_text = stdout(run(tmp.path(), &["gates", "run", "--all"]));
    assert!(cached_text.contains("(cached)"));
    assert!(cached_text.contains(&format!("(tree {})", &second_tree[..12])));
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 2);

    let bypassed_json = stdout(run(
        tmp.path(),
        &["gates", "run", "--all", "--no-cache", "--json"],
    ));
    let bypassed: serde_json::Value = serde_json::from_str(&bypassed_json).unwrap();
    assert_eq!(bypassed[0]["cached"], false);
    assert_eq!(bypassed[0]["tree_hash"], second_tree);
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 3);

    let refreshed_json = stdout(run(tmp.path(), &["gates", "run", "--all", "--json"]));
    let refreshed: serde_json::Value = serde_json::from_str(&refreshed_json).unwrap();
    assert_eq!(refreshed[0]["cached"], true);
    assert_eq!(refreshed[0]["tree_hash"], second_tree);
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 3);

    let adopted = stdout(run(
        tmp.path(),
        &["adopt", "--task", "session gates", "--json"],
    ));
    let session: serde_json::Value = serde_json::from_str(&adopted).unwrap();
    let session_id = session["id"].as_i64().unwrap().to_string();
    let session_bypass = stdout(run(
        tmp.path(),
        &[
            "gates",
            "run",
            "--session",
            &session_id,
            "--no-cache",
            "--json",
        ],
    ));
    let session_bypass: serde_json::Value = serde_json::from_str(&session_bypass).unwrap();
    assert_eq!(session_bypass[0]["cached"], false);
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 4);

    let session_cached = stdout(run(
        tmp.path(),
        &["gates", "run", "--session", &session_id, "--json"],
    ));
    let session_cached: serde_json::Value = serde_json::from_str(&session_cached).unwrap();
    assert_eq!(session_cached[0]["cached"], true);
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 4);
}

#[test]
fn pre_push_adapter_proves_the_clean_outgoing_head_and_refuses_drift() {
    let tmp = fixture();
    let head = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    let update = format!(
        "refs/heads/main {head} refs/heads/main {}\n",
        "0".repeat(40)
    );

    let verified = stdout(run_with_stdin(
        tmp.path(),
        &["gates", "pre-push", "origin", "unused-url", "--json"],
        &update,
    ));
    let report: serde_json::Value = serde_json::from_str(&verified).unwrap();
    assert_eq!(report["plan"]["remote"], "origin");
    assert_eq!(report["plan"]["pushed_sha"], head);
    assert_eq!(report["plan"]["updates"].as_array().unwrap().len(), 1);
    assert_eq!(report["gate_outcomes"][0]["status"], "pass");
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 1);

    std::fs::write(tmp.path().join("tracked.txt"), "dirty\n").unwrap();
    let dirty = run_with_stdin(
        tmp.path(),
        &["gates", "pre-push", "origin", "unused-url"],
        &update,
    );
    assert!(!dirty.status.success());
    assert!(String::from_utf8_lossy(&dirty.stderr).contains("requires a clean checkout"));
    assert_eq!(run_count(&tmp.path().join("gate-runs.txt")), 1);

    let wrong_tip = format!(
        "refs/heads/main {} refs/heads/main {}\n",
        "a".repeat(40),
        "0".repeat(40)
    );
    let mismatch = run_with_stdin(
        tmp.path(),
        &["gates", "pre-push", "origin", "unused-url"],
        &wrong_tip,
    );
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("is not this checkout's HEAD"));

    let multiple_tips = format!(
        "refs/heads/a {} refs/heads/a {}\nrefs/heads/b {} refs/heads/b {}\n",
        "a".repeat(40),
        "0".repeat(40),
        "b".repeat(40),
        "0".repeat(40),
    );
    let ambiguous = run_with_stdin(
        tmp.path(),
        &["gates", "pre-push", "origin", "unused-url"],
        &multiple_tips,
    );
    assert!(!ambiguous.status.success());
    assert!(
        String::from_utf8_lossy(&ambiguous.stderr)
            .contains("cannot prove multiple different local tips")
    );

    let empty = run_with_stdin(
        tmp.path(),
        &["gates", "pre-push", "origin", "unused-url"],
        "",
    );
    assert!(!empty.status.success());
    assert!(String::from_utf8_lossy(&empty.stderr).contains("received no ref updates"));

    let deletion = format!("(delete) {} refs/heads/old {head}\n", "0".repeat(40));
    let deleted = stdout(run_with_stdin(
        tmp.path(),
        &["gates", "pre-push", "origin", "unused-url", "--json"],
        &deletion,
    ));
    let report: serde_json::Value = serde_json::from_str(&deleted).unwrap();
    assert!(report["plan"]["pushed_sha"].is_null());
    assert!(report["gate_outcomes"].as_array().unwrap().is_empty());
}

#[test]
fn submit_cli_reports_an_unchanged_worktree_submission_as_a_noop() {
    let tmp = fixture();
    let worktree = tmp.path().join(".aethyme/worktrees/noop-submit");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/noop-submit",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    let adopted = stdout(run(
        &worktree,
        &["adopt", "--task", "inspect only", "--json"],
    ));
    let session: serde_json::Value = serde_json::from_str(&adopted).unwrap();
    let session_id = session["id"].as_i64().unwrap().to_string();

    let json = stdout(run(
        &worktree,
        &["submit", "--session", &session_id, "--json"],
    ));
    let outcome: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(outcome["no_changes"], true);
    assert_eq!(outcome["promoted"], false);
    assert_eq!(outcome["entry"]["status"], "superseded");
    assert_eq!(outcome["gate_outcomes"], serde_json::json!([]));
    assert!(!tmp.path().join("gate-runs.txt").exists());

    let text = stdout(run(&worktree, &["submit", "--session", &session_id]));
    assert!(text.contains("no pending session-owned content"));
    assert!(text.contains("integration was not moved and no gates ran"));
    assert!(!text.contains("gate wall time"));
}

#[test]
fn submit_cli_bypasses_and_then_refreshes_the_merged_tree_cache() {
    let tmp = fixture();
    let counter = tmp.path().join("submit-runs.txt");
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[promote]\nmode = \"manual\"\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        format!(
            "[[gate]]\nname = \"submit-cache\"\ncommand = \"echo run >> '{}'\"\n",
            counter.display()
        ),
    )
    .unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "configure submit gate"]);

    let worktree = tmp.path().join(".aethyme/worktrees/submit-cache");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/submit-cache",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    let adopted = stdout(run(
        &worktree,
        &["adopt", "--task", "submit cache", "--json"],
    ));
    let session: serde_json::Value = serde_json::from_str(&adopted).unwrap();
    let session_id = session["id"].as_i64().unwrap().to_string();
    std::fs::write(worktree.join("tracked.txt"), "submitted\n").unwrap();
    git(&worktree, &["add", "tracked.txt"]);
    git(&worktree, &["commit", "-qm", "submitted change"]);

    let first = stdout(run(
        &worktree,
        &["submit", "--session", &session_id, "--json"],
    ));
    let first: serde_json::Value = serde_json::from_str(&first).unwrap();
    assert_eq!(first["gate_outcomes"][0]["cached"], false);
    assert_eq!(run_count(&counter), 1);

    let cached = stdout(run(
        &worktree,
        &["submit", "--session", &session_id, "--json"],
    ));
    let cached: serde_json::Value = serde_json::from_str(&cached).unwrap();
    assert_eq!(cached["gate_outcomes"][0]["cached"], true);
    assert_eq!(run_count(&counter), 1);

    let bypassed = stdout(run(
        &worktree,
        &["submit", "--session", &session_id, "--no-cache", "--json"],
    ));
    let bypassed: serde_json::Value = serde_json::from_str(&bypassed).unwrap();
    assert_eq!(bypassed["gate_outcomes"][0]["cached"], false);
    assert_eq!(run_count(&counter), 2);

    let refreshed = stdout(run(
        &worktree,
        &["submit", "--session", &session_id, "--json"],
    ));
    let refreshed: serde_json::Value = serde_json::from_str(&refreshed).unwrap();
    assert_eq!(refreshed["gate_outcomes"][0]["cached"], true);
    assert_eq!(run_count(&counter), 2);
}

#[test]
fn independent_repositories_share_gate_host_resources_and_release_them() {
    let first = fixture();
    let second = fixture();
    let state = tempfile::tempdir().unwrap();
    let config = r#"
[[gate]]
name = "host-resources"
command = "test -n \"$AETHYME_RESOURCE_DOCKER_PROJECT\" && test \"$AETHYME_RESOURCE_DATABASE\" = host-test-shared && sleep 6"
cache = false
resource_ttl_seconds = 15

[[gate.resources]]
key = "docker_project"
kind = "namespace"
prefix = "gate-test"

[[gate.resources]]
key = "database"
kind = "exclusive_key"
name = "host-test-shared"
"#;
    for repo in [first.path(), second.path()] {
        std::fs::write(repo.join(".aethyme/gates.toml"), config).unwrap();
        git(repo, &["add", ".aethyme/gates.toml"]);
        git(repo, &["commit", "-qm", "configure host resource gate"]);
    }

    let running = Command::new(CLI)
        .args(["gates", "run", "--all", "--no-cache", "--json"])
        .current_dir(first.path())
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let db_path = state.path().join("host-resources.db");
    let mut observed = false;
    for _ in 0..100 {
        if db_path.exists()
            && aethyme_broker::HostResourceCoordinator::open_read_only(&db_path)
                .unwrap()
                .list(false)
                .unwrap()
                .len()
                == 1
        {
            observed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(observed, "first gate never acquired its host bundle");

    let blocked = Command::new(CLI)
        .args(["gates", "run", "--all", "--no-cache", "--json"])
        .current_dir(second.path())
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .output()
        .unwrap();
    assert!(!blocked.status.success());
    let blocked_json: serde_json::Value = serde_json::from_slice(&blocked.stdout).unwrap();
    assert_eq!(blocked_json[0]["status"], "error");
    assert_eq!(blocked_json[0]["failure_class"], "resource_contention");
    assert!(blocked_json[0].get("resource_lease").is_none());

    let completed = running.wait_with_output().unwrap();
    assert!(
        completed.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&completed.stdout),
        String::from_utf8_lossy(&completed.stderr)
    );
    let completed_json: serde_json::Value = serde_json::from_slice(&completed.stdout).unwrap();
    assert_eq!(completed_json[0]["status"], "pass");
    assert_eq!(
        completed_json[0]["resource_lease"]["allocations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        completed_json[0]["definition_hash"].as_str().unwrap().len(),
        64
    );
    let initial_expiry = completed_json[0]["resource_lease"]["expires_at"]
        .as_i64()
        .unwrap();
    let inventory = aethyme_broker::HostResourceCoordinator::open_read_only(&db_path)
        .unwrap()
        .list(true)
        .unwrap();
    assert!(
        inventory[0].expires_at > initial_expiry,
        "long-running gate did not renew its host resource lease"
    );
    assert!(
        aethyme_broker::HostResourceCoordinator::open_read_only(&db_path)
            .unwrap()
            .list(false)
            .unwrap()
            .is_empty(),
        "successful gate did not release its host resources"
    );

    let failing_config = config.replace(" && sleep 6", " && false");
    std::fs::write(first.path().join(".aethyme/gates.toml"), failing_config).unwrap();
    let failed = Command::new(CLI)
        .args(["gates", "run", "--all", "--no-cache", "--json"])
        .current_dir(first.path())
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(
        aethyme_broker::HostResourceCoordinator::open_read_only(&db_path)
            .unwrap()
            .list(false)
            .unwrap()
            .is_empty(),
        "failing gate did not release its host resources"
    );
}
