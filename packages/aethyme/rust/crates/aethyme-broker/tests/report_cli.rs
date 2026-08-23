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

fn sorted_keys(value: &serde_json::Value) -> Vec<String> {
    let mut keys = value
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    keys
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

#[test]
fn list_and_show_have_stable_json_with_digest_and_filing_state() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let capture = run(
        tmp.path(),
        &[
            "report",
            "capture",
            "--kind",
            "bug",
            "--title",
            "Inspectable failure",
            "--output",
            "inspectable.json",
        ],
    );
    assert!(capture.status.success());
    let report_path = tmp.path().join(".aethyme/reports/inspectable.json");
    let digest = sha256(&std::fs::read(&report_path).unwrap());

    let listed = run(tmp.path(), &["report", "list", "--json"]);
    assert!(listed.status.success());
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(
        sorted_keys(&listed),
        ["invalid", "reports", "schema_version"]
    );
    assert_eq!(listed["schema_version"], 1);
    assert_eq!(listed["invalid"].as_array().unwrap().len(), 0);
    let summary = &listed["reports"][0];
    assert_eq!(
        sorted_keys(summary),
        [
            "captured_at",
            "digest",
            "filing_state",
            "kind",
            "path",
            "report_schema_version",
            "title",
            "version",
        ]
    );
    assert_eq!(summary["path"], ".aethyme/reports/inspectable.json");
    assert_eq!(summary["kind"], "bug");
    assert_eq!(summary["digest"], digest);
    assert_eq!(summary["filing_state"], "unfiled");
    assert!(summary["captured_at"].as_i64().is_some());
    assert!(
        summary["version"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );

    std::fs::write(
        tmp.path().join(".aethyme/reports/.filings.json"),
        serde_json::json!({
            "schema_version": 1,
            "filings": { digest.clone(): { "issue": 46 } }
        })
        .to_string(),
    )
    .unwrap();
    let shown = run(
        tmp.path(),
        &["report", "show", "inspectable.json", "--json"],
    );
    assert!(shown.status.success());
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(sorted_keys(&shown), ["report", "schema_version", "summary"]);
    assert_eq!(shown["schema_version"], 1);
    assert_eq!(shown["summary"]["digest"], digest);
    assert_eq!(shown["summary"]["filing_state"], "filed");
    assert_eq!(shown["report"]["title"], "Inspectable failure");
    assert_eq!(shown["report"]["kind"], "bug");

    let mut changed = std::fs::read(&report_path).unwrap();
    changed.push(b' ');
    std::fs::write(&report_path, changed).unwrap();
    let changed = run(
        tmp.path(),
        &["report", "show", "inspectable.json", "--json"],
    );
    assert!(changed.status.success());
    let changed: serde_json::Value = serde_json::from_slice(&changed.stdout).unwrap();
    assert_ne!(changed["summary"]["digest"], digest);
    assert_eq!(changed["summary"]["filing_state"], "unfiled");
}

#[test]
fn list_is_read_only_and_reports_corruption_without_hiding_valid_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let empty = run(tmp.path(), &["report", "list", "--json"]);
    assert!(empty.status.success());
    assert!(
        !tmp.path().join(".aethyme/broker.db").exists(),
        "read-only inventory created broker state"
    );

    assert!(
        run(
            tmp.path(),
            &[
                "report",
                "capture",
                "--kind",
                "improvement",
                "--title",
                "Healthy report",
                "--output",
                "healthy.json",
            ],
        )
        .status
        .success()
    );
    std::fs::write(
        tmp.path().join(".aethyme/reports/corrupt.json"),
        "not json\n",
    )
    .unwrap();
    let listed = run(tmp.path(), &["report", "list", "--json"]);
    assert!(listed.status.success());
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed["reports"].as_array().unwrap().len(), 1);
    assert_eq!(listed["invalid"].as_array().unwrap().len(), 1);
    assert_eq!(
        listed["invalid"][0]["path"],
        ".aethyme/reports/corrupt.json"
    );

    let mut tampered: serde_json::Value = serde_json::from_slice(
        &std::fs::read(tmp.path().join(".aethyme/reports/healthy.json")).unwrap(),
    )
    .unwrap();
    tampered["snapshot"]["gates"] = serde_json::json!([{
        "gate": "test",
        "tree_hash": "0123456789abcdef",
        "status": "pass",
        "failure_class": null,
        "cache_source": "executed",
        "exit_code": 0,
        "duration_ms": 1,
        "recorded_at": 1,
        "triggered_by": "/private/secret.rs"
    }]);
    std::fs::write(
        tmp.path().join(".aethyme/reports/tampered.json"),
        serde_json::to_vec(&tampered).unwrap(),
    )
    .unwrap();
    let listed = run(tmp.path(), &["report", "list", "--json"]);
    assert!(listed.status.success());
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed["reports"].as_array().unwrap().len(), 1);
    assert_eq!(listed["invalid"].as_array().unwrap().len(), 2);
    assert!(listed["invalid"].as_array().unwrap().iter().any(|entry| {
        entry["path"] == ".aethyme/reports/tampered.json"
            && entry["error"]
                .as_str()
                .is_some_and(|error| error.contains("non-repository-relative"))
    }));

    let shown = run(tmp.path(), &["report", "show", "corrupt.json", "--json"]);
    assert!(!shown.status.success());
    assert!(String::from_utf8_lossy(&shown.stderr).contains("invalid report JSON"));
}

#[test]
fn list_order_is_newest_first_with_path_tiebreakers() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    assert!(
        run(
            tmp.path(),
            &[
                "report",
                "capture",
                "--kind",
                "bug",
                "--title",
                "Ordering fixture",
                "--output",
                "base.json",
            ],
        )
        .status
        .success()
    );
    let base_path = tmp.path().join(".aethyme/reports/base.json");
    let mut report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&base_path).unwrap()).unwrap();
    std::fs::remove_file(base_path).unwrap();
    for (name, captured_at) in [("z.json", 100), ("a.json", 100), ("newest.json", 200)] {
        report["captured_at"] = captured_at.into();
        std::fs::write(
            tmp.path().join(".aethyme/reports").join(name),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
    }
    for name in ["z-invalid", "a-invalid"] {
        std::fs::write(tmp.path().join(".aethyme/reports").join(name), "invalid").unwrap();
    }

    let output = run(tmp.path(), &["report", "list", "--json"]);
    assert!(output.status.success());
    let output: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let paths = output["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|report| report["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        [
            ".aethyme/reports/newest.json",
            ".aethyme/reports/a.json",
            ".aethyme/reports/z.json",
        ]
    );
    let invalid = output["invalid"]
        .as_array()
        .unwrap()
        .iter()
        .map(|report| report["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        invalid,
        [".aethyme/reports/a-invalid", ".aethyme/reports/z-invalid",]
    );
}

#[cfg(unix)]
#[test]
fn show_rejects_path_escape_and_symlinked_artifacts() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".aethyme/reports")).unwrap();
    std::fs::write(tmp.path().join("outside.json"), "{}\n").unwrap();
    symlink(
        tmp.path().join("outside.json"),
        tmp.path().join(".aethyme/reports/link.json"),
    )
    .unwrap();

    for selector in ["../outside.json", "/tmp/outside.json", "nested/out.json"] {
        let output = run(tmp.path(), &["report", "show", selector, "--json"]);
        assert!(!output.status.success(), "accepted {selector}");
    }
    let linked = run(tmp.path(), &["report", "show", "link.json", "--json"]);
    assert!(!linked.status.success());
    assert!(String::from_utf8_lossy(&linked.stderr).contains("regular file"));
}
