//! Routine reporting must identify uninspected data; audits stay explicit.
use std::process::Command;
const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

fn fixture() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(repo.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.path().join(".gitignore"), ".aethyme/\n").unwrap();
    for args in [
        vec!["add", "README.md", ".gitignore"],
        vec!["commit", "-qm", "fixture"],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .status()
                .unwrap()
                .success()
        );
    }
    repo
}

fn status(repo: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = common::broker_cli(CLI, args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn cli_routine_and_explicit_audit_have_distinct_freshness_contracts() {
    let repo = fixture();
    let routine = status(repo.path(), &["status", "--json"]);
    assert_eq!(routine["leases_refreshed"], false);
    assert_eq!(routine["cleanup_retention"]["eligibility_checked"], false);
    assert!(
        routine["deferred_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "unpushed_commits")
    );
    let audited = status(repo.path(), &["status", "--refresh", "--json"]);
    assert_eq!(audited["leases_refreshed"], true);
    assert_eq!(audited["cleanup_retention"]["eligibility_checked"], true);
    assert!(audited["deferred_checks"].as_array().unwrap().is_empty());
    let brief = status(repo.path(), &["status", "--summary", "--json"]);
    assert_eq!(brief["leases_refreshed"], false);
    assert_eq!(
        brief["leases_refreshed_at_ms"],
        audited["leases_refreshed_at_ms"]
    );
    assert!(
        brief["deferred_checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "git_refs")
    );
    assert!(brief["phase_timings_ms"]["open"].is_number());
}

#[test]
fn cli_rejects_summary_with_refresh_and_refresh_on_another_command() {
    let repo = fixture();
    for args in [
        vec!["status", "--summary", "--refresh"],
        vec!["events", "--refresh"],
    ] {
        let output = common::broker_cli(CLI, &args)
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("--refresh"));
    }
}
