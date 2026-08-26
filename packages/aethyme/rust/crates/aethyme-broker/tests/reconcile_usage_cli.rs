use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::Broker;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const RESOURCES_USAGE: &str =
    "usage: aethyme broker resources reconcile <lease-id> --confirm <generation> [--json]";
const OPERATIONS_USAGE: &str = "usage: aethyme broker operations reconcile --operation <id> \
     --outcome <succeeded|failed> --reason <text> [--json]";
const INTEGRATION_USAGE: &str = "usage: aethyme broker integration reconcile --upstream <ref> \
     [--resolution-file <path>] [--dry-run | --apply --confirm <sha256>] [--json]";

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository(repo: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    git(repo, &["add", "tracked.txt"]);
    git(repo, &["commit", "-qm", "initial"]);
}

fn run(repo: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .env("AETHYME_HOST_STATE_DIR", state)
        .output()
        .unwrap()
}

fn assert_usage(output: Output, expected: &str) {
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(expected), "missing full usage in: {stderr}");
}

#[test]
fn every_reconcile_usage_error_includes_the_complete_required_contract() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    repository(&repo);
    let state = temp.path().join("host-state");

    for args in [
        &["resources", "reconcile"][..],
        &["resources", "reconcile", "lease-1"][..],
        &["resources", "reconcile", "lease-1", "--confirm", "bad"][..],
    ] {
        assert_usage(run(&repo, &state, args), RESOURCES_USAGE);
    }

    for args in [
        &["operations", "reconcile"][..],
        &["operations", "reconcile", "--operation", "1"][..],
        &[
            "operations",
            "reconcile",
            "--operation",
            "1",
            "--outcome",
            "unknown",
        ][..],
        &[
            "operations",
            "reconcile",
            "--operation",
            "1",
            "--outcome",
            "failed",
        ][..],
    ] {
        assert_usage(run(&repo, &state, args), OPERATIONS_USAGE);
    }

    for args in [
        &["integration", "reconcile"][..],
        &["integration", "reconcile", "--upstream", "HEAD", "--apply"][..],
        &[
            "integration",
            "reconcile",
            "--upstream",
            "HEAD",
            "--apply",
            "--dry-run",
        ][..],
    ] {
        assert_usage(run(&repo, &state, args), INTEGRATION_USAGE);
    }
}

#[cfg(unix)]
#[test]
fn originating_github_write_prints_the_complete_reconciliation_handoff() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    repository(&repo);
    let state = temp.path().join("host-state");
    let mut broker = Broker::open(&repo)
        .unwrap()
        .with_host_operation_database(state.join("host-operations.db"));
    let session = broker.adopt(&repo, None).unwrap();
    drop(broker);

    let bin = temp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\nprintf 'ambiguous write failure\\n' >&2\nexit 1\n",
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
    let session_id = session.id.to_string();
    let output = Command::new(CLI)
        .args([
            "gh",
            "--session",
            &session_id,
            "--repo",
            "Owner/Repo",
            "--reason",
            "exercise unknown outcome recovery",
            "--json",
            "--",
            "issue",
            "create",
            "--title",
            "fixture",
            "--body",
            "fixture",
        ])
        .current_dir(&repo)
        .env("AETHYME_HOST_STATE_DIR", &state)
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["operation"]["repository"], "github.com/owner/repo");
    assert_eq!(report["operation"]["status"], "outcome_unknown");
    let operation_id = report["operation"]["id"].as_i64().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    for required in [
        "Canonical repository github.com/owner/repo is now write-blocked".to_string(),
        format!("Operation ID: {operation_id}"),
        "Inspect GitHub state for canonical repository github.com/owner/repo".to_string(),
        format!("--operation {operation_id} --outcome succeeded --reason"),
        format!("--operation {operation_id} --outcome failed --reason"),
        "Blind retry is forbidden".to_string(),
    ] {
        assert!(
            stderr.contains(&required),
            "missing {required:?} in: {stderr}"
        );
    }
}
