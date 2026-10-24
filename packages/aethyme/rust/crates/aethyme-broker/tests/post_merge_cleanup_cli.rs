#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::MergeStatus;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

fn git(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

/// Resolve the `git` the fake `git` below delegates to, skipping wrappers.
///
/// `run_merge` prepends the fake bin directory to `PATH`. A `git` that is
/// itself a script re-resolving `git` through `PATH` — the shape every
/// developer-environment git wrapper takes — would exec straight back into
/// the fake, and the two would trade `exec` calls forever inside a single
/// process: no output, no crash, just a hung test. Only a real executable
/// terminates the chain, so accept nothing that starts with `#!`.
fn real_git() -> String {
    use std::io::Read;

    std::env::split_paths(&std::env::var_os("PATH").expect("PATH"))
        .map(|dir| dir.join("git"))
        .find(|candidate| {
            let Ok(mut file) = std::fs::File::open(candidate) else {
                return false;
            };
            let mut magic = [0u8; 2];
            file.read_exact(&mut magic).is_ok() && &magic != b"#!"
        })
        .expect("git on PATH")
        .display()
        .to_string()
}

fn prepend_path(directory: &Path) -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    format!("{}:{current}", directory.display())
}

fn write_executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn run_merge(
    repo: &Path,
    fake_bin: &Path,
    fixture_owner: &Path,
    session_id: i64,
    upstream_sha: &str,
    race_target: Option<&str>,
) -> Output {
    common::broker_cli_for_fixture(
        CLI,
        &[
            "advanced",
            "gh",
            "--session",
            &session_id.to_string(),
            "--repo",
            "acme/product",
            "--reason",
            "test-authorized pull request merge",
            "--json",
            "--",
            "pr",
            "merge",
            "42",
            "--merge",
        ],
        repo,
    )
    .env("PATH", prepend_path(fake_bin))
    .env("AETHYME_TEST_REAL_GIT", real_git())
    .env("AETHYME_TEST_UPSTREAM_SHA", upstream_sha)
    .env("AETHYME_TEST_RACE_TARGET", race_target.unwrap_or(""))
    .env("AETHYME_TEST_RACE_MARKER", fixture_owner.join("cas-raced"))
    .env(
        "AETHYME_BROKER_OPEN_WORKTREE_ROOT",
        fixture_owner.join("worktrees"),
    )
    .current_dir(repo)
    .output()
    .unwrap()
}

#[test]
fn successful_coordinated_pr_merge_cleans_a_fully_landed_integration_layer() {
    run_landing_case(None);
}

#[test]
fn concurrent_exact_target_advance_finishes_the_prepared_reconciliation() {
    run_landing_case(Some(true));
}

#[test]
fn concurrent_different_target_keeps_its_ref_and_queue_unchanged() {
    run_landing_case(Some(false));
}

fn run_landing_case(same_target_race: Option<bool>) {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let fake_bin = temp.path().join("fake-bin");
    std::fs::create_dir(&repo).unwrap();
    std::fs::create_dir(&fake_bin).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Aethyme Test"]);
    git(
        &repo,
        &["config", "user.email", "aethyme-test@example.invalid"],
    );
    std::fs::create_dir(repo.join("src")).unwrap();
    std::fs::write(repo.join(".gitignore"), ".aethyme/\n").unwrap();
    std::fs::write(repo.join("src/service.txt"), "feature=off\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "initial"]);
    let initial = git(&repo, &["rev-parse", "HEAD"]);
    git(
        &repo,
        &["remote", "add", "origin", "git@github.com:acme/product.git"],
    );
    git(&repo, &["update-ref", "refs/remotes/origin/main", &initial]);
    git(&repo, &["config", "branch.main.remote", "origin"]);
    git(&repo, &["config", "branch.main.merge", "refs/heads/main"]);

    let remote = temp.path().join("origin.git");
    git(
        &repo,
        &["clone", "--bare", "-q", ".", remote.to_str().unwrap()],
    );
    let ssh = temp.path().join("fixture-ssh");
    write_executable(
        &ssh,
        r#"#!/bin/sh
case "$*" in
  *git-upload-pack*) exec git-upload-pack "$(dirname "$0")/origin.git" ;;
  *git-receive-pack*) exec git-receive-pack "$(dirname "$0")/origin.git" ;;
esac
exit 64
"#,
    );
    let quoted_ssh = ssh.to_str().unwrap().replace('\'', "'\\''");
    let ssh_command = format!("sh '{quoted_ssh}'");
    git(&repo, &["config", "core.sshCommand", &ssh_command]);

    let mut broker = common::open_fixture_broker(&repo, temp.path()).unwrap();
    let session = broker
        .start_worktree("land through pull request", None)
        .unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("src/service.txt"), "feature=on\n").unwrap();
    git(&worktree, &["add", "src/service.txt"]);
    git(&worktree, &["commit", "-qm", "enable feature"]);
    let promoted = broker.submit(session.id).unwrap();
    assert!(promoted.promoted);
    let promotion =
        serde_json::from_str::<serde_json::Value>(promoted.entry.details_json.as_deref().unwrap())
            .unwrap()["commit"]
            .as_str()
            .unwrap()
            .to_string();

    git(&repo, &["switch", "-qc", "upstream-landing", "main"]);
    std::fs::write(repo.join("src/service.txt"), "feature=on\n").unwrap();
    git(&repo, &["add", "src/service.txt"]);
    git(&repo, &["commit", "-qm", "merge pull request 42"]);
    let upstream = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["switch", "main"]);
    drop(broker);

    write_executable(
        &fake_bin.join("gh"),
        r#"#!/bin/sh
if [ "$1 $2" = 'pr view' ]; then
  printf '{"headRefName":"agent/pr-42","headRefOid":"1111111111111111111111111111111111111111","baseRefName":"main"}\n'
  exit 0
fi
printf 'merged pull request 42\n'
exit 0
"#,
    );
    write_executable(
        &fake_bin.join("git"),
        r#"#!/bin/sh
if [ "$1" = "fetch" ]; then
  "$AETHYME_TEST_REAL_GIT" update-ref refs/remotes/origin/main "$AETHYME_TEST_UPSTREAM_SHA"
  exit $?
fi
if [ -n "$AETHYME_TEST_RACE_TARGET" ] && [ "$1" = "update-ref" ] && [ "$2" = "refs/heads/aethyme/integration" ] && [ "$3" = "$AETHYME_TEST_UPSTREAM_SHA" ] && [ ! -e "$AETHYME_TEST_RACE_MARKER" ]; then
  "$AETHYME_TEST_REAL_GIT" update-ref "$2" "$AETHYME_TEST_RACE_TARGET" "$4" || exit $?
  : > "$AETHYME_TEST_RACE_MARKER"
fi
exec "$AETHYME_TEST_REAL_GIT" "$@"
"#,
    );

    let race_target = same_target_race.map(|same| {
        if same {
            upstream.as_str()
        } else {
            initial.as_str()
        }
    });
    let output = run_merge(
        &repo,
        &fake_bin,
        temp.path(),
        session.id,
        &upstream,
        race_target,
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["operation"]["status"], "succeeded");
    assert_eq!(
        temp.path().join("cas-raced").exists(),
        same_target_race.is_some(),
        "the requested interleaving must actually run"
    );
    if same_target_race == Some(false) {
        assert_eq!(report["post_merge_cleanup"]["state"], "deferred");
        assert_eq!(git(&repo, &["rev-parse", "aethyme/integration"]), initial);
        let mut broker = common::open_fixture_broker(&repo, temp.path()).unwrap();
        let entry = broker
            .store()
            .merge_queue()
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == promoted.entry.id)
            .unwrap();
        assert_eq!(entry.status, MergeStatus::Promoted);
        return;
    }
    assert_eq!(report["post_merge_cleanup"]["state"], "cleaned");
    assert_eq!(
        report["post_merge_cleanup"]["cleanup"]["cleaned_queue_entry_ids"],
        serde_json::json!([promoted.entry.id])
    );
    assert!(
        report["post_merge_cleanup"]["fetch_operation_id"]
            .as_i64()
            .is_some()
    );

    assert_eq!(git(&repo, &["rev-parse", "origin/main"]), upstream);
    assert_eq!(git(&repo, &["rev-parse", "aethyme/integration"]), upstream);
    assert_ne!(promotion, upstream);
    let mut broker = common::open_fixture_broker(&repo, temp.path()).unwrap();
    let entry = broker
        .store()
        .merge_queue()
        .unwrap()
        .into_iter()
        .find(|entry| entry.id == promoted.entry.id)
        .unwrap();
    assert_eq!(entry.status, MergeStatus::ExternallyLanded);
}
