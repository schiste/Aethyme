use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) {
    git_output(repo, args);
}

fn git_output(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "README.md", ".gitignore"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    tmp
}

#[test]
fn adopt_cli_distinguishes_created_and_reused_session_identities() {
    let tmp = fixture();

    let created = stdout(&run(tmp.path(), &["adopt", "--task", "first"]));
    assert!(
        created.contains("Created session 1 on the existing worktree"),
        "{created}"
    );

    stdout(&run(tmp.path(), &["close", "--session", "1"]));
    let created_after_close = stdout(&run(tmp.path(), &["adopt", "--reuse", "--task", "second"]));
    assert!(
        created_after_close.contains("Created session 2 on the existing worktree"),
        "{created_after_close}"
    );
    assert!(!created_after_close.contains("Reusing session"));

    let reused = stdout(&run(tmp.path(), &["adopt", "--reuse", "--task", "third"]));
    assert!(reused.contains("Reusing session 2"), "{reused}");
}

#[test]
fn adopt_cli_exposes_structured_reuse_drift_and_safe_guidance() {
    let tmp = fixture();
    stdout(&run(tmp.path(), &["adopt", "--task", "first"]));

    git(tmp.path(), &["checkout", "-qb", "integration-work"]);
    std::fs::write(tmp.path().join("shared.txt"), "integration\n").unwrap();
    git(tmp.path(), &["add", "shared.txt"]);
    git(tmp.path(), &["commit", "-qm", "integration advances"]);
    let integration_head = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    git(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
    git(tmp.path(), &["checkout", "-q", "main"]);
    let session_head = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    std::fs::write(tmp.path().join("shared.txt"), "dirty session edit\n").unwrap();

    let json = stdout(&run(
        tmp.path(),
        &["adopt", "--reuse", "--task", "follow-up", "--json"],
    ));
    let report: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(report["outcome"], "reused");
    assert_eq!(report["integration_drift"]["session_head"], session_head);
    assert_eq!(
        report["integration_drift"]["integration_head"],
        integration_head
    );
    assert_eq!(report["integration_drift"]["relation"], "behind");
    assert_eq!(report["integration_drift"]["ahead_commits"], 0);
    assert_eq!(report["integration_drift"]["behind_commits"], 1);
    assert_eq!(
        report["integration_drift"]["overlapping_changed_paths"],
        serde_json::json!(["shared.txt"])
    );
    assert!(report["integration_drift"]["warning"].is_string());
    assert_eq!(
        report["integration_drift"]["safe_next_action"],
        "aethyme broker integration status"
    );

    let rendered = stdout(&run(
        tmp.path(),
        &["adopt", "--reuse", "--task", "render drift"],
    ));
    assert!(rendered.contains("Integration drift: behind"), "{rendered}");
    assert!(rendered.contains("Overlapping changed paths:\n  shared.txt"));
    assert!(rendered.contains("Warning:"), "{rendered}");
    assert!(
        rendered.contains("Safe next action: aethyme broker integration status"),
        "{rendered}"
    );
}

#[test]
fn adopt_cli_syncs_reuse_to_integration_and_exposes_the_exact_transition() {
    let tmp = fixture();
    stdout(&run(tmp.path(), &["adopt", "--task", "first"]));
    let session_head = git_output(tmp.path(), &["rev-parse", "HEAD"]);

    git(tmp.path(), &["checkout", "-qb", "integration-work"]);
    std::fs::write(tmp.path().join("integration.txt"), "integration\n").unwrap();
    git(tmp.path(), &["add", "integration.txt"]);
    git(tmp.path(), &["commit", "-qm", "integration advances"]);
    let integration_head = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    git(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
    git(tmp.path(), &["checkout", "-q", "main"]);

    let json = stdout(&run(
        tmp.path(),
        &[
            "adopt",
            "--reuse",
            "--sync-integration",
            "--task",
            "synchronized follow-up",
            "--json",
        ],
    ));
    let report: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(report["integration_sync"]["outcome"], "fast_forwarded");
    assert_eq!(report["integration_sync"]["before_head"], session_head);
    assert_eq!(report["integration_sync"]["after_head"], integration_head);
    assert_eq!(report["diff_base"], integration_head);
    assert_eq!(report["integration_drift"]["relation"], "current");
    assert_eq!(
        git_output(tmp.path(), &["rev-parse", "HEAD"]),
        integration_head
    );

    let rendered = stdout(&run(
        tmp.path(),
        &[
            "adopt",
            "--reuse",
            "--sync-integration",
            "--task",
            "current",
        ],
    ));
    assert!(
        rendered.contains("Integration synchronization: already current"),
        "{rendered}"
    );
}

#[test]
fn adopt_cli_requires_reuse_for_integration_sync() {
    let tmp = fixture();
    let output = run(tmp.path(), &["adopt", "--sync-integration"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--sync-integration requires --reuse")
    );
}
