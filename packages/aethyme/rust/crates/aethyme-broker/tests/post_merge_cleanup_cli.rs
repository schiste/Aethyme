#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{Broker, MergeStatus};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

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

fn real_git() -> String {
    let output = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
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
    host_state: &Path,
    session_id: i64,
    upstream_sha: &str,
) -> Output {
    Command::new(CLI)
        .args([
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
        ])
        .current_dir(repo)
        .env("PATH", prepend_path(fake_bin))
        .env("AETHYME_TEST_REAL_GIT", real_git())
        .env("AETHYME_TEST_UPSTREAM_SHA", upstream_sha)
        .env("AETHYME_HOST_STATE_DIR", host_state)
        .output()
        .unwrap()
}

#[test]
fn successful_coordinated_pr_merge_cleans_a_fully_landed_integration_layer() {
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
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/product.git",
        ],
    );
    git(&repo, &["update-ref", "refs/remotes/origin/main", &initial]);
    git(&repo, &["config", "branch.main.remote", "origin"]);
    git(&repo, &["config", "branch.main.merge", "refs/heads/main"]);

    let mut broker = Broker::open(&repo).unwrap();
    let session = broker.start_worktree("land through pull request").unwrap();
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
        "#!/bin/sh\nprintf 'merged pull request 42\\n'\nexit 0\n",
    );
    write_executable(
        &fake_bin.join("git"),
        r#"#!/bin/sh
if [ "$1" = "fetch" ]; then
  "$AETHYME_TEST_REAL_GIT" update-ref refs/remotes/origin/main "$AETHYME_TEST_UPSTREAM_SHA"
  exit $?
fi
exec "$AETHYME_TEST_REAL_GIT" "$@"
"#,
    );

    let output = run_merge(
        &repo,
        &fake_bin,
        &temp.path().join("host-state"),
        session.id,
        &upstream,
    );
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["operation"]["status"], "succeeded");
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
    let mut broker = Broker::open(&repo).unwrap();
    let entry = broker
        .store()
        .merge_queue()
        .unwrap()
        .into_iter()
        .find(|entry| entry.id == promoted.entry.id)
        .unwrap();
    assert_eq!(entry.status, MergeStatus::ExternallyLanded);
}
