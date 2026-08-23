use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::Broker;
use sha2::{Digest, Sha256};

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

fn init_repo(repo: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-qm", "init"]);
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn digest_from(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .find_map(|line| line.strip_prefix("SHA-256: "))
        .unwrap()
        .to_string()
}

#[test]
fn capture_writes_ignored_report_atomically_and_prints_exact_digest() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let scaffold = run(tmp.path(), &["scaffold"]);
    assert!(scaffold.status.success());
    assert!(
        std::fs::read_to_string(tmp.path().join(".gitignore"))
            .unwrap()
            .contains(".aethyme/reports/")
    );

    let output = run(
        tmp.path(),
        &[
            "report",
            "capture",
            "--kind",
            "bug",
            "--title",
            "Gate failed",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let relative = stdout
        .lines()
        .find_map(|line| line.strip_prefix("Captured bug report: "))
        .unwrap();
    let digest = stdout
        .lines()
        .find_map(|line| line.strip_prefix("SHA-256: "))
        .unwrap();
    let bytes = std::fs::read(tmp.path().join(relative)).unwrap();
    assert_eq!(digest, sha256(&bytes));
    let report: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(report["kind"], "bug");
    assert_eq!(report["title"], "Gate failed");
    assert_eq!(report["snapshot"]["includes_task"], false);
    assert!(
        std::fs::read_dir(tmp.path().join(".aethyme/reports"))
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".report-"))
    );
}

#[test]
fn stdout_is_the_exact_digest_byte_stream_and_does_not_create_a_report_file() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());

    let output = run(
        tmp.path(),
        &[
            "report",
            "capture",
            "--kind",
            "improvement",
            "--title",
            "Quieter diagnostics",
            "--stdout",
        ],
    );
    assert!(output.status.success());
    assert_eq!(digest_from(&output.stderr), sha256(&output.stdout));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["kind"], "improvement");
    assert!(!tmp.path().join(".aethyme/reports").exists());
}

#[test]
fn explicit_output_never_overwrites_and_cannot_escape_report_directory() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let args = [
        "report",
        "capture",
        "--kind",
        "bug",
        "--title",
        "Stable failure",
        "--output",
        "reviewed.json",
    ];
    assert!(run(tmp.path(), &args).status.success());
    let first = std::fs::read(tmp.path().join(".aethyme/reports/reviewed.json")).unwrap();
    let collision = run(tmp.path(), &args);
    assert!(!collision.status.success());
    assert!(String::from_utf8_lossy(&collision.stderr).contains("already exists"));
    assert_eq!(
        std::fs::read(tmp.path().join(".aethyme/reports/reviewed.json")).unwrap(),
        first
    );

    for destination in [
        "../escape.json",
        "/tmp/aethyme-escape.json",
        "nested/out.json",
    ] {
        let output = run(
            tmp.path(),
            &[
                "report",
                "capture",
                "--kind",
                "bug",
                "--title",
                "Escape attempt",
                "--output",
                destination,
            ],
        );
        assert!(!output.status.success(), "accepted {destination}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid report output"));
    }
}

#[test]
fn task_text_is_inferred_from_the_current_session_only_with_opt_in() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    broker.adopt(tmp.path(), Some("TASK-TEXT-SECRET")).unwrap();
    drop(broker);

    let base = [
        "report",
        "capture",
        "--kind",
        "bug",
        "--title",
        "Session failure",
        "--stdout",
    ];
    let redacted = run(tmp.path(), &base);
    assert!(redacted.status.success());
    assert!(!String::from_utf8_lossy(&redacted.stdout).contains("TASK-TEXT-SECRET"));

    let mut included = base.to_vec();
    included.push("--include-task");
    let included = run(tmp.path(), &included);
    assert!(included.status.success());
    assert!(String::from_utf8_lossy(&included.stdout).contains("TASK-TEXT-SECRET"));
}
