use std::path::Path;
use std::process::{Command, Output};

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
    git(tmp.path(), &["add", "README.md"]);
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
