use std::path::Path;
use std::process::{Command, Output};

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
