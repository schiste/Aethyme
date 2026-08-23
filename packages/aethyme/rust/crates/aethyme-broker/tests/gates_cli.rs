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
        ".aethyme/broker.db*\n.aethyme/logs/\n.aethyme/run/\n.aethyme/worktrees/\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"provenance\"\ncommand = \"true\"\n",
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

#[test]
fn gate_cli_reports_tree_provenance_for_executed_and_cached_results() {
    let tmp = fixture();
    let first_tree = tree_hash(tmp.path());

    let executed_text = stdout(run(tmp.path(), &["gates", "run", "--all"]));
    assert!(executed_text.contains(&format!("(tree {})", &first_tree[..12])));
    assert!(!executed_text.contains("(cached)"));

    let cached_json = stdout(run(tmp.path(), &["gates", "run", "--all", "--json"]));
    let cached: serde_json::Value = serde_json::from_str(&cached_json).unwrap();
    assert_eq!(cached[0]["cached"], true);
    assert_eq!(cached[0]["tree_hash"], first_tree);

    std::fs::write(tmp.path().join("tracked.txt"), "second\n").unwrap();
    let second_tree = tree_hash(tmp.path());
    assert_ne!(second_tree, first_tree);

    let executed_json = stdout(run(tmp.path(), &["gates", "run", "--all", "--json"]));
    let executed: serde_json::Value = serde_json::from_str(&executed_json).unwrap();
    assert_eq!(executed[0]["cached"], false);
    assert_eq!(executed[0]["tree_hash"], second_tree);

    let cached_text = stdout(run(tmp.path(), &["gates", "run", "--all"]));
    assert!(cached_text.contains("(cached)"));
    assert!(cached_text.contains(&format!("(tree {})", &second_tree[..12])));
}
