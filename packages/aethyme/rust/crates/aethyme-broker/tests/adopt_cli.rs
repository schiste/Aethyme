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
fn start_cli_returns_deterministic_planned_leases_and_status_exposes_them() {
    let tmp = fixture();
    let started = stdout(&run(
        tmp.path(),
        &[
            "start",
            "--task",
            "planned rewrite",
            "--path",
            "zeta.txt",
            "--path",
            "generated/",
            "--path",
            "zeta.txt",
            "--json",
        ],
    ));
    let value: serde_json::Value = serde_json::from_str(&started).unwrap();
    assert_eq!(value["start_base"]["ref_name"], "refs/heads/main");
    assert_eq!(value["start_base"]["evidence"], "conventional_main");
    assert_eq!(
        value["planned_explicit_leases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|lease| lease["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["generated/", "zeta.txt"]
    );

    let status = stdout(&run(tmp.path(), &["status", "--json"]));
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(
        status["leases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|lease| lease["kind"] == "explicit")
            .count(),
        2
    );

    let conflict = run(
        tmp.path(),
        &[
            "start",
            "--task",
            "conflicting rewrite",
            "--path",
            "generated/policy.md",
            "--json",
        ],
    );
    assert!(!conflict.status.success());
    assert!(
        String::from_utf8_lossy(&conflict.stderr).contains("planned lease"),
        "{}",
        String::from_utf8_lossy(&conflict.stderr)
    );
}

#[test]
fn start_selects_integration_or_default_branch_without_using_checkout_head() {
    for checkout in ["main", "feature", "detached"] {
        let tmp = fixture();
        let main = git_output(tmp.path(), &["rev-parse", "HEAD"]);
        if checkout != "main" {
            git(tmp.path(), &["switch", "-qc", "feature"]);
            std::fs::write(tmp.path().join("feature.txt"), checkout).unwrap();
            git(tmp.path(), &["add", "feature.txt"]);
            git(tmp.path(), &["commit", "-qm", "throwaway feature"]);
        }
        if checkout == "detached" {
            git(tmp.path(), &["checkout", "--detach", "HEAD"]);
        }

        let started = stdout(&run(tmp.path(), &["start", "--task", checkout, "--json"]));
        let value: serde_json::Value = serde_json::from_str(&started).unwrap();
        assert_eq!(value["start_base"]["ref_name"], "refs/heads/main");
        assert_eq!(value["start_base"]["commit"], main);
        assert_eq!(value["diff_base"], main);
    }

    let tmp = fixture();
    git(tmp.path(), &["switch", "-qc", "promoted"]);
    std::fs::write(tmp.path().join("promoted.txt"), "integration\n").unwrap();
    git(tmp.path(), &["add", "promoted.txt"]);
    git(tmp.path(), &["commit", "-qm", "promoted work"]);
    let integration = git_output(tmp.path(), &["rev-parse", "HEAD"]);
    git(
        tmp.path(),
        &["update-ref", "refs/heads/aethyme/integration", &integration],
    );
    let started = stdout(&run(
        tmp.path(),
        &["start", "--task", "from integration", "--json"],
    ));
    let value: serde_json::Value = serde_json::from_str(&started).unwrap();
    assert_eq!(
        value["start_base"]["ref_name"],
        "refs/heads/aethyme/integration"
    );
    assert_eq!(value["start_base"]["commit"], integration);
    assert_eq!(value["start_base"]["evidence"], "integration_tip");
}

#[test]
fn start_refuses_ambiguous_or_missing_default_refs() {
    let ambiguous = fixture();
    git(ambiguous.path(), &["branch", "master", "main"]);
    let output = run(ambiguous.path(), &["start", "--task", "ambiguous default"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("both refs/heads/main and refs/heads/master exist"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let missing = fixture();
    git(missing.path(), &["branch", "-m", "feature-only"]);
    let output = run(missing.path(), &["start", "--task", "missing default"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no integration tip"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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

/// Uncommitted work already in the checkout is not this session's, but a
/// repository pre-push gate validates the whole snapshot, so it fails the
/// session's first push for reasons the session cannot see (#131 finding 3).
#[test]
fn adopt_warns_about_uncommitted_paths_it_did_not_create() {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);

    // Clean checkout: nothing to warn about.
    let clean = run(repo.path(), &["adopt", "--task", "clean checkout"]);
    let clean_out = String::from_utf8_lossy(&clean.stdout);
    assert!(
        !clean_out.contains("warning:"),
        "a clean checkout must not warn: {clean_out}"
    );
    run(repo.path(), &["close", "--session", "1"]);

    // Pre-existing unrelated work: the session must be told.
    std::fs::write(repo.path().join("unrelated.txt"), "not mine\n").unwrap();
    let dirty = run(repo.path(), &["adopt", "--task", "dirty checkout"]);
    let dirty_out = String::from_utf8_lossy(&dirty.stdout);
    assert!(
        dirty_out.contains("warning:") && dirty_out.contains("unrelated.txt"),
        "adopt must name the pre-existing uncommitted paths: {dirty_out}"
    );
    assert!(
        dirty_out.contains("pre-push"),
        "the warning must say why it matters: {dirty_out}"
    );
}
