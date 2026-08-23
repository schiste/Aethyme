use std::path::Path;
use std::process::{Command, Output};

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

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
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
