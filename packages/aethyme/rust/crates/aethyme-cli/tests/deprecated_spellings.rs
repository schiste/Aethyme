//! Removed v0.8.8 spellings fail with a replacement hint; canonical routes stay usable.

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn aethyme(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(args)
        .current_dir(cwd)
        .env_remove("AETHYME_ROOT")
        .env_remove("AETHYME_BROKER_DB")
        .env_remove("AETHYME_REPO")
        .env_remove("AETHYME_AGENT")
        .env("XDG_CONFIG_HOME", cwd.join("empty-config"))
        .stdin(Stdio::null())
        .output()
        .expect("run aethyme")
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    tmp
}

fn assert_removed(output: &Output, old: &str, new: &str) {
    assert_eq!(
        output.status.code(),
        Some(2),
        "expected removed-spelling refusal for {old}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = format!("Error: '{old}' was removed in v0.8.8; use '{new}'");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        expected,
        "unexpected refusal for {old}"
    );
    assert!(output.stdout.is_empty(), "removed route printed to stdout");
}

#[test]
fn removed_adopt_spellings_fail_with_specific_replacements() {
    let tmp = fixture();
    let repo = tmp.path().join("repo");
    for (args, old, new) in [
        (
            &["broker", "adopt", "--task", "old", "--json"][..],
            "aethyme broker adopt",
            "aethyme broker start --adopt",
        ),
        (
            &["broker", "adopt", "--reuse", "--task", "again", "--json"][..],
            "aethyme broker adopt --reuse",
            "aethyme broker start --reuse",
        ),
    ] {
        let output = aethyme(&repo, args);
        assert_removed(&output, old, new);
    }
    assert!(
        !repo.join(".aethyme").exists(),
        "removed spellings must fail before creating broker state"
    );
}

#[test]
fn removed_merged_and_advanced_spellings_fail_with_replacements() {
    let tmp = fixture();
    let repo = tmp.path().join("repo");
    for (args, old, new) in [
        (
            &["broker", "blockers", "--json"][..],
            "aethyme broker blockers",
            "aethyme broker unblock",
        ),
        (
            &["broker", "reclaim", "plan", "--json"][..],
            "aethyme broker reclaim",
            "aethyme broker gc reclaim",
        ),
        (
            &["broker", "promotion-record", "plan", "--json"][..],
            "aethyme broker promotion-record",
            "aethyme broker submit promotion-record",
        ),
        (
            &["broker", "leases", "--json"][..],
            "aethyme broker leases",
            "aethyme broker advanced leases",
        ),
    ] {
        let output = aethyme(&repo, args);
        assert_removed(&output, old, new);
    }
}

#[test]
fn removed_top_level_spellings_fail_and_canonical_routes_stay_usable() {
    let tmp = fixture();
    let repo = tmp.path().join("repo");

    assert_removed(
        &aethyme(&repo, &["readiness", "--json"]),
        "aethyme readiness",
        "aethyme broker status readiness",
    );
    let readiness = aethyme(&repo, &["broker", "status", "readiness", "--json"]);
    assert!(
        readiness.status.success(),
        "{}",
        String::from_utf8_lossy(&readiness.stderr)
    );

    assert_removed(
        &aethyme(&repo, &["broker", "certify", "--json"]),
        "aethyme broker certify",
        "aethyme certify",
    );
    let certify = aethyme(&repo, &["certify", "--json"]);
    assert!(
        certify.status.success(),
        "{}",
        String::from_utf8_lossy(&certify.stderr)
    );

    assert_removed(
        &aethyme(&repo, &["enhance", "verify", "--repo", "."]),
        "aethyme enhance verify",
        "aethyme deploy verify --generated-only",
    );
}

#[test]
fn installed_hook_entry_points_remain_available_without_deprecation() {
    let tmp = fixture();
    let repo = tmp.path().join("repo");
    let output = aethyme(&repo, &["broker", "hooks", "post-commit"]);
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("is deprecated"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn removed_public_spellings_and_advanced_duplicates_refuse_before_preflight() {
    let tmp = fixture();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    std::fs::write(
        repo.join(".aethyme/repository.json"),
        "{\"schema_version\":999}\n",
    )
    .unwrap();

    for (args, old, new) in [
        (
            &["broker", "doctor", "--json"][..],
            "aethyme broker doctor",
            "aethyme broker status doctor",
        ),
        (
            &["broker", "promote", "--entry", "1", "--json"][..],
            "aethyme broker promote",
            "aethyme broker submit promote",
        ),
    ] {
        assert_removed(&aethyme(&repo, args), old, new);
    }

    for line in [
        "broker status doctor --json",
        "broker submit promote --entry 1 --json",
    ] {
        let args: Vec<&str> = line.split(' ').collect();
        let output = aethyme(&repo, &args);
        assert_eq!(
            output.status.code(),
            Some(1),
            "`{line}` must be refused by compatibility policy: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    for (line, public) in [
        (
            "broker advanced status doctor --json",
            "status doctor --json",
        ),
        (
            "broker advanced status readiness recover --plan x",
            "status readiness recover --plan x",
        ),
        (
            "broker advanced submit promote --entry 1 --json",
            "submit promote --entry 1 --json",
        ),
        (
            "broker advanced submit prepare --session 1",
            "submit prepare --session 1",
        ),
    ] {
        let args: Vec<&str> = line.split(' ').collect();
        let output = aethyme(&repo, &args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "`{line}`: {stderr}");
        assert!(
            stderr.contains(&format!("use 'aethyme broker {public}'")),
            "`{line}`: {stderr}"
        );
        assert!(output.stdout.is_empty(), "`{line}` printed on stdout");
    }
}
