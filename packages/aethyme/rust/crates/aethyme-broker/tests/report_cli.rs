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

fn run_with_env(repo: &Path, args: &[&str], environment: &[(&str, &str)]) -> Output {
    let mut command = Command::new(CLI);
    command.args(args).current_dir(repo);
    for (key, value) in environment {
        command.env(key, value);
    }
    command.output().unwrap()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn capture_report(repo: &Path, kind: &str, title: &str, filename: &str) {
    let output = run(
        repo,
        &[
            "report", "capture", "--kind", kind, "--title", title, "--output", filename,
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_issue_form(repo: &Path, filename: &str, source: &str) {
    let directory = repo.join(".github/ISSUE_TEMPLATE");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join(filename), source).unwrap();
}

fn write_reviewed_issue_artifact(repo: &Path, filename: &str) -> Vec<u8> {
    capture_report(repo, "bug", "Reviewed gate failure", "source.json");
    write_issue_form(
        repo,
        "fileable.yml",
        r#"name: Fileable Bug
title: "[Bug]: "
body:
  - type: textarea
    id: summary
    attributes:
      label: Summary
    validations:
      required: true
  - type: textarea
    id: environment
    attributes:
      label: Environment
    validations:
      required: true
"#,
    );
    let mut args = vec!["report", "render", "source.json", "--form", "fileable.yml"];
    if filename.ends_with(".issue.md") {
        args.extend(["--output", filename]);
    } else {
        args.push("--json");
    }
    let rendered = run(repo, &args);
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    let path = repo.join(".aethyme/reports").join(filename);
    if filename.ends_with(".issue.md") {
        std::fs::read(path).unwrap()
    } else {
        std::fs::write(path, &rendered.stdout).unwrap();
        rendered.stdout
    }
}

#[cfg(unix)]
fn install_fake_gh(repo: &Path) -> (String, String, String, String) {
    use std::os::unix::fs::PermissionsExt;

    let bin = repo.join("fake-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        r#"#!/bin/sh
printf '%s\n' "$@" > "$AETHYME_FAKE_GH_ARGS"
printf 'called\n' >> "$AETHYME_FAKE_GH_CALLS"
body_file=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--body-file' ]; then
    shift
    body_file="$1"
  fi
  shift
done
if [ -n "$body_file" ]; then
  cp "$body_file" "$AETHYME_FAKE_GH_BODY"
fi
case "$AETHYME_FAKE_GH_MODE" in
  fail)
    printf 'simulated ambiguous failure\n' >&2
    exit 1
    ;;
  malformed)
    printf 'created without an issue identity\n'
    exit 0
    ;;
esac
printf 'https://github.com/owner/repo/issues/42\n'
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&gh).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&gh, permissions).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (
        path,
        repo.join("gh-args").to_string_lossy().into_owned(),
        repo.join("gh-calls").to_string_lossy().into_owned(),
        repo.join("gh-body").to_string_lossy().into_owned(),
    )
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

#[test]
fn render_preserves_form_order_and_reports_required_unfilled_fields() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    capture_report(
        tmp.path(),
        "bug",
        "Gate failed deterministically",
        "bug.json",
    );
    write_issue_form(
        tmp.path(),
        "bug.yml",
        r#"name: Bug Report
title: "[Bug]: "
body:
  - type: markdown
    attributes:
      value: Review the generated diagnostic before filing.
  - type: textarea
    id: summary
    attributes:
      label: Summary
    validations:
      required: true
  - type: textarea
    id: reproduction
    attributes:
      label: Reproduction
    validations:
      required: true
  - type: textarea
    id: environment
    attributes:
      label: Environment
    validations:
      required: true
"#,
    );

    for name in ["broker.db", "broker.db-shm", "broker.db-wal"] {
        let path = tmp.path().join(".aethyme").join(name);
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
    }
    let output = run(
        tmp.path(),
        &["report", "render", "bug.json", "--form", "bug.yml"],
    );
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let intro = stdout.find("Review the generated diagnostic").unwrap();
    let summary = stdout.find("## Summary").unwrap();
    let reproduction = stdout.find("## Reproduction").unwrap();
    let environment = stdout.find("## Environment").unwrap();
    assert!(intro < summary && summary < reproduction && reproduction < environment);
    assert!(stdout.contains("Gate failed deterministically"));
    assert!(stdout.contains("Unfilled: no allowlisted report value maps to `reproduction`"));
    assert!(stdout.contains("- OS:"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Issue title: [Bug]: Gate failed deterministically"));
    assert!(stderr.contains("required issue-form fields remain unfilled: reproduction"));
    assert!(
        !tmp.path().join(".aethyme/broker.db").exists(),
        "offline render recreated broker state"
    );
}

#[test]
fn render_json_maps_only_known_fields_and_validates_dropdown_options() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    capture_report(tmp.path(), "bug", "Inspectable failure", "report.json");
    write_issue_form(
        tmp.path(),
        "complete.yml",
        r#"name: Complete Form
body:
  - type: input
    id: summary
    attributes:
      label: Summary
    validations:
      required: true
  - type: dropdown
    id: report_kind
    attributes:
      label: Kind
      options:
        - Bug
        - Improvement
    validations:
      required: true
  - type: input
    id: report_digest
    attributes:
      label: Digest
    validations:
      required: true
  - type: textarea
    id: environment
    attributes:
      label: Environment
    validations:
      required: true
  - type: textarea
    id: notes_for_maintainer
    attributes:
      label: Maintainer Notes
"#,
    );

    let output = run(
        tmp.path(),
        &[
            "report",
            "render",
            ".aethyme/reports/report.json",
            "--form",
            ".github/ISSUE_TEMPLATE/complete.yml",
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rendered: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rendered["schema_version"], 1);
    assert_eq!(rendered["valid"], true);
    assert_eq!(rendered["missing_required"], serde_json::json!([]));
    assert_eq!(rendered["fields"][0]["status"], "mapped");
    assert_eq!(rendered["fields"][1]["status"], "mapped");
    assert_eq!(rendered["fields"][4]["status"], "unfilled");
    assert!(
        rendered["markdown"]
            .as_str()
            .unwrap()
            .contains("## Kind\n\nBug")
    );
    assert!(
        rendered["markdown"]
            .as_str()
            .unwrap()
            .contains("`notes_for_maintainer`")
    );
}

#[test]
fn required_unknown_and_checkbox_fields_remain_explicitly_unfilled() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    capture_report(
        tmp.path(),
        "improvement",
        "Better diagnostics",
        "report.json",
    );
    write_issue_form(
        tmp.path(),
        "unknown.yml",
        r#"name: Unknown Fields
body:
  - type: textarea
    id: proposal
    attributes:
      label: Proposal
    validations:
      required: true
  - type: checkboxes
    id: terms
    attributes:
      label: Terms
      options:
        - label: I reviewed the report
          required: true
  - type: future-widget
    id: future
    attributes:
      label: Future Field
"#,
    );

    let output = run(
        tmp.path(),
        &[
            "report",
            "render",
            "report.json",
            "--form",
            "unknown.yml",
            "--json",
        ],
    );
    assert!(!output.status.success());
    let rendered: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rendered["valid"], false);
    assert_eq!(
        rendered["missing_required"],
        serde_json::json!(["proposal", "terms"])
    );
    assert_eq!(rendered["fields"][0]["kind"], "textarea");
    assert_eq!(rendered["fields"][1]["kind"], "checkboxes");
    assert_eq!(rendered["fields"][2]["kind"], "unknown");
    let markdown = rendered["markdown"].as_str().unwrap();
    assert!(markdown.contains("## Proposal"));
    assert!(markdown.contains("## Terms"));
    assert!(markdown.contains("## Future Field"));
}

#[test]
fn render_rejects_malformed_forms_and_path_escapes() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    capture_report(tmp.path(), "bug", "Invalid form", "report.json");
    write_issue_form(tmp.path(), "broken.yml", "name: [\n");

    let malformed = run(
        tmp.path(),
        &["report", "render", "report.json", "--form", "broken.yml"],
    );
    assert!(!malformed.status.success());
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("invalid repository issue form"));

    for selector in [
        "../broken.yml",
        "/tmp/broken.yml",
        "nested/broken.yml",
        "broken.yaml",
    ] {
        let output = run(
            tmp.path(),
            &["report", "render", "report.json", "--form", selector],
        );
        assert!(!output.status.success(), "accepted {selector}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid issue form path"));
    }
}

#[cfg(unix)]
#[test]
fn render_rejects_symlinked_issue_forms() {
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    capture_report(tmp.path(), "bug", "Linked form", "report.json");
    write_issue_form(tmp.path(), "outside.yml", "name: Outside\nbody: []\n");
    symlink(
        tmp.path().join(".github/ISSUE_TEMPLATE/outside.yml"),
        tmp.path().join(".github/ISSUE_TEMPLATE/link.yml"),
    )
    .unwrap();

    let output = run(
        tmp.path(),
        &["report", "render", "report.json", "--form", "link.yml"],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must not be a symbolic link"));
}

#[cfg(unix)]
#[test]
fn file_uses_the_confirmed_render_and_journals_the_returned_issue() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    broker.adopt(tmp.path(), Some("file report")).unwrap();
    drop(broker);
    let artifact = write_reviewed_issue_artifact(tmp.path(), "reviewed.issue.md");
    let digest = sha256(&artifact);
    let expected_body = std::str::from_utf8(&artifact)
        .unwrap()
        .split_once("\n-->\n")
        .unwrap()
        .1;
    let source = run(tmp.path(), &["report", "show", "source.json", "--json"]);
    let source: serde_json::Value = serde_json::from_slice(&source.stdout).unwrap();
    let (path, args_path, calls_path, body_path) = install_fake_gh(tmp.path());

    let output = run_with_env(
        tmp.path(),
        &[
            "report",
            "file",
            ".aethyme/reports/reviewed.issue.md",
            "--repo",
            "owner/repo",
            "--confirm",
            &digest,
            "--json",
        ],
        &[
            ("PATH", &path),
            ("AETHYME_FAKE_GH_ARGS", &args_path),
            ("AETHYME_FAKE_GH_CALLS", &calls_path),
            ("AETHYME_FAKE_GH_BODY", &body_path),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let filed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(filed["state"], "filed");
    assert_eq!(filed["digest"], digest);
    assert_eq!(filed["report_digest"], source["summary"]["digest"]);
    assert_eq!(filed["repository"], "owner/repo");
    assert_eq!(filed["issue_number"], 42);
    assert_eq!(
        filed["issue_url"],
        "https://github.com/owner/repo/issues/42"
    );
    assert_eq!(std::fs::read_to_string(&body_path).unwrap(), expected_body);
    let args = std::fs::read_to_string(&args_path).unwrap();
    assert!(args.contains("issue\ncreate\n--title\n[Bug]: Reviewed gate failure"));
    assert!(args.contains("--body-file"));

    let mut broker = Broker::open(tmp.path()).unwrap();
    let operation = broker
        .store()
        .coordinated_operations()
        .unwrap()
        .into_iter()
        .last()
        .unwrap();
    assert_eq!(operation.status, aethyme_broker::OperationStatus::Succeeded);
    let details: serde_json::Value =
        serde_json::from_str(operation.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(details["result"]["issue_number"], 42);
    assert_eq!(
        details["result"]["issue_url"],
        "https://github.com/owner/repo/issues/42"
    );
    assert_eq!(details["result"]["reviewed_digest"], digest);
    assert!(!operation.command_json.contains("Reviewed gate failure"));
    assert!(!operation.command_json.contains(".report-file-"));
    drop(broker);

    let listed = run(tmp.path(), &["report", "list", "--json"]);
    assert!(listed.status.success());
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed["reports"].as_array().unwrap().len(), 1);
    assert_eq!(listed["invalid"].as_array().unwrap().len(), 0);
    assert_eq!(listed["reports"][0]["filing_state"], "filed");

    let duplicate = run_with_env(
        tmp.path(),
        &[
            "report",
            "file",
            "reviewed.issue.md",
            "--repo",
            "owner/repo",
            "--confirm",
            &digest,
        ],
        &[
            ("PATH", &path),
            ("AETHYME_FAKE_GH_ARGS", &args_path),
            ("AETHYME_FAKE_GH_CALLS", &calls_path),
            ("AETHYME_FAKE_GH_BODY", &body_path),
        ],
    );
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("already filed"));
    assert_eq!(
        std::fs::read_to_string(&calls_path)
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn file_refuses_digest_drift_before_invoking_github() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    broker.adopt(tmp.path(), None).unwrap();
    drop(broker);
    let artifact = write_reviewed_issue_artifact(tmp.path(), "drift.issue.json");
    let confirmed = sha256(&artifact);
    let path_to_artifact = tmp.path().join(".aethyme/reports/drift.issue.json");
    let mut changed = artifact;
    changed.push(b' ');
    std::fs::write(path_to_artifact, changed).unwrap();
    let (path, args_path, calls_path, body_path) = install_fake_gh(tmp.path());

    let output = run_with_env(
        tmp.path(),
        &[
            "report",
            "file",
            "drift.issue.json",
            "--repo",
            "owner/repo",
            "--confirm",
            &confirmed,
        ],
        &[
            ("PATH", &path),
            ("AETHYME_FAKE_GH_ARGS", &args_path),
            ("AETHYME_FAKE_GH_CALLS", &calls_path),
            ("AETHYME_FAKE_GH_BODY", &body_path),
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("changed after confirmation"));
    assert!(!Path::new(&calls_path).exists());
    let mut broker = Broker::open(tmp.path()).unwrap();
    assert!(broker.store().coordinated_operations().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn file_refuses_required_unfilled_sections_before_invoking_github() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    broker.adopt(tmp.path(), None).unwrap();
    drop(broker);
    capture_report(tmp.path(), "bug", "Needs reproduction", "source.json");
    write_issue_form(
        tmp.path(),
        "required.yml",
        r#"name: Required Form
body:
  - type: textarea
    id: reproduction
    attributes:
      label: Reproduction
    validations:
      required: true
"#,
    );
    let rendered = run(
        tmp.path(),
        &[
            "report",
            "render",
            "source.json",
            "--form",
            "required.yml",
            "--json",
        ],
    );
    assert!(!rendered.status.success());
    std::fs::write(
        tmp.path().join(".aethyme/reports/unfilled.issue.json"),
        &rendered.stdout,
    )
    .unwrap();
    let digest = sha256(&rendered.stdout);
    let (path, args_path, calls_path, body_path) = install_fake_gh(tmp.path());

    let output = run_with_env(
        tmp.path(),
        &[
            "report",
            "file",
            "unfilled.issue.json",
            "--repo",
            "owner/repo",
            "--confirm",
            &digest,
        ],
        &[
            ("PATH", &path),
            ("AETHYME_FAKE_GH_ARGS", &args_path),
            ("AETHYME_FAKE_GH_CALLS", &calls_path),
            ("AETHYME_FAKE_GH_BODY", &body_path),
        ],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("required unfilled fields: reproduction")
    );
    assert!(!Path::new(&calls_path).exists());

    let artifact_path = tmp.path().join(".aethyme/reports/unfilled.issue.json");
    let mut reviewed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&artifact_path).unwrap()).unwrap();
    reviewed["markdown"] = reviewed["markdown"]
        .as_str()
        .unwrap()
        .replace(
            "> Unfilled: no allowlisted report value maps to `reproduction` (Textarea).",
            "1. Run `aethyme broker gates run`.\n2. Observe the failure.",
        )
        .into();
    let reviewed = serde_json::to_vec_pretty(&reviewed).unwrap();
    std::fs::write(&artifact_path, &reviewed).unwrap();
    let reviewed_digest = sha256(&reviewed);
    let filed = run_with_env(
        tmp.path(),
        &[
            "report",
            "file",
            "unfilled.issue.json",
            "--repo",
            "owner/repo",
            "--confirm",
            &reviewed_digest,
        ],
        &[
            ("PATH", &path),
            ("AETHYME_FAKE_GH_ARGS", &args_path),
            ("AETHYME_FAKE_GH_CALLS", &calls_path),
            ("AETHYME_FAKE_GH_BODY", &body_path),
        ],
    );
    assert!(
        filed.status.success(),
        "{}",
        String::from_utf8_lossy(&filed.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&calls_path)
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn ambiguous_file_outcome_requires_reconciliation_and_is_never_retried() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    broker.adopt(tmp.path(), None).unwrap();
    drop(broker);
    let artifact = write_reviewed_issue_artifact(tmp.path(), "ambiguous.issue.json");
    let digest = sha256(&artifact);
    let (path, args_path, calls_path, body_path) = install_fake_gh(tmp.path());
    let environment = [
        ("PATH", path.as_str()),
        ("AETHYME_FAKE_GH_ARGS", args_path.as_str()),
        ("AETHYME_FAKE_GH_CALLS", calls_path.as_str()),
        ("AETHYME_FAKE_GH_BODY", body_path.as_str()),
        ("AETHYME_FAKE_GH_MODE", "fail"),
    ];
    let args = [
        "report",
        "file",
        "ambiguous.issue.json",
        "--repo",
        "owner/repo",
        "--confirm",
        digest.as_str(),
        "--json",
    ];

    let first = run_with_env(tmp.path(), &args, &environment);
    assert!(!first.status.success());
    let result: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(result["state"], "reconciliation_required");
    assert_eq!(result["operation_status"], "outcome_unknown");
    assert!(String::from_utf8_lossy(&first.stderr).contains("do not retry"));
    let operation_id = result["operation_id"].as_i64().unwrap();

    let second = run_with_env(tmp.path(), &args, &environment);
    assert!(!second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("has an unknown outcome"));
    assert!(stderr.contains(&format!("--operation {operation_id}")));
    assert_eq!(
        std::fs::read_to_string(&calls_path)
            .unwrap()
            .lines()
            .count(),
        1
    );

    let mut broker = Broker::open(tmp.path()).unwrap();
    broker
        .reconcile_coordinated_operation(operation_id, true, "external inspection found the issue")
        .unwrap();
    drop(broker);
    let after_successful_reconciliation = run_with_env(tmp.path(), &args, &environment);
    assert!(!after_successful_reconciliation.status.success());
    assert!(
        String::from_utf8_lossy(&after_successful_reconciliation.stderr)
            .contains("already has completed filing operation")
    );
    assert_eq!(
        std::fs::read_to_string(&calls_path)
            .unwrap()
            .lines()
            .count(),
        1
    );

    let shown = run(tmp.path(), &["report", "show", "source.json", "--json"]);
    assert!(shown.status.success());
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["summary"]["filing_state"], "unfiled");
}

#[cfg(unix)]
#[test]
fn successful_command_without_a_parseable_issue_url_is_ambiguous() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    broker.adopt(tmp.path(), None).unwrap();
    drop(broker);
    let artifact = write_reviewed_issue_artifact(tmp.path(), "malformed.issue.json");
    let digest = sha256(&artifact);
    let (path, args_path, calls_path, body_path) = install_fake_gh(tmp.path());

    let output = run_with_env(
        tmp.path(),
        &[
            "report",
            "file",
            "malformed.issue.json",
            "--repo",
            "owner/repo",
            "--confirm",
            &digest,
            "--json",
        ],
        &[
            ("PATH", &path),
            ("AETHYME_FAKE_GH_ARGS", &args_path),
            ("AETHYME_FAKE_GH_CALLS", &calls_path),
            ("AETHYME_FAKE_GH_BODY", &body_path),
            ("AETHYME_FAKE_GH_MODE", "malformed"),
        ],
    );
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["state"], "reconciliation_required");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let operation = broker
        .store()
        .coordinated_operation(result["operation_id"].as_i64().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        operation.status,
        aethyme_broker::OperationStatus::OutcomeUnknown
    );
    assert!(
        operation
            .details_json
            .as_deref()
            .unwrap()
            .contains("success_result_not_recorded")
    );
}
