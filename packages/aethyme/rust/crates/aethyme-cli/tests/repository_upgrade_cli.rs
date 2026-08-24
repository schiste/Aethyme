use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_testkit::{aethyme_bin, tmp_dir};
use serde_json::Value;

fn repository(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    let initialized = Command::new("git")
        .args(["init", "-b", "main"])
        .arg(&repo)
        .output()
        .unwrap();
    assert!(initialized.status.success());
    commit_all(&repo, "initial");
    repo
}

fn command(repo: &Path) -> Command {
    let mut command = Command::new(aethyme_bin());
    command
        .current_dir(repo)
        .env_remove("AETHYME_ROOT")
        .env("XDG_CONFIG_HOME", repo.join("empty-config"));
    command
}

fn commit_all(repo: &Path, message: &str) {
    let added = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(added.status.success());
    let committed = Command::new("git")
        .args([
            "-c",
            "user.name=Aethyme Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            message,
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        committed.status.success(),
        "{}",
        String::from_utf8_lossy(&committed.stderr)
    );
}

fn run(repo: &Path, args: &[&str]) -> Output {
    command(repo).args(args).output().unwrap()
}

fn old_canonical_deployment(repo: &Path) {
    let deployed = run(repo, &["deploy", "--repo", repo.to_str().unwrap()]);
    assert!(
        deployed.status.success(),
        "{}",
        String::from_utf8_lossy(&deployed.stderr)
    );
    fs::remove_file(repo.join(".aethyme/repository.json")).unwrap();
    commit_all(repo, "deploy old repository contract");
}

fn json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn canonical_upgrade_is_read_only_then_digest_bound_and_verifiable() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);

    let before = run(&repo, &["upgrade", "plan", "--json"]);
    let plan = json(&before);
    assert_eq!(plan["from_schema"], 0);
    assert_eq!(plan["to_schema"], 1);
    assert_eq!(plan["safe"], true);
    assert_eq!(plan["applied"], false);
    assert_eq!(
        plan["migrations"],
        serde_json::json!(["repository-deployment-v1"])
    );
    assert_eq!(git_status(&repo), "");
    assert!(!String::from_utf8_lossy(&before.stdout).contains(repo.to_string_lossy().as_ref()));

    let rejected = run(
        &repo,
        &["upgrade", "apply", "--confirm", &"0".repeat(64), "--json"],
    );
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("changed after review"));
    assert_eq!(git_status(&repo), "");

    let digest = plan["plan_digest"].as_str().unwrap();
    let applied = run(&repo, &["upgrade", "apply", "--confirm", digest, "--json"]);
    let report = json(&applied);
    assert_eq!(report["applied"], true);
    assert_eq!(report["from_schema"], 0);
    assert_eq!(
        report["migrations"],
        serde_json::json!(["repository-deployment-v1"])
    );
    assert_eq!(report["plan_digest"], digest);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(repo.join(".aethyme/repository.json")).unwrap())
            .unwrap()["schema_version"],
        1
    );

    let verified = run(
        &repo,
        &["deploy", "verify", "--repo", repo.to_str().unwrap()],
    );
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
}

#[test]
fn dirty_repository_blocks_upgrade_without_mutation() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);
    fs::write(repo.join("README.md"), "locally edited\n").unwrap();

    let plan = json(&run(&repo, &["upgrade", "plan", "--json"]));
    assert_eq!(plan["safe"], false);
    assert!(
        plan["blockers"][0]
            .as_str()
            .unwrap()
            .contains("worktree is dirty")
    );

    let digest = plan["plan_digest"].as_str().unwrap();
    let applied = run(&repo, &["upgrade", "apply", "--confirm", digest]);
    assert!(!applied.status.success());
    assert!(!repo.join(".aethyme/repository.json").exists());
}

#[test]
fn broker_refuses_an_obsolete_enrolled_repository() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);

    let status = run(&repo, &["broker", "status", "--json"]);
    assert!(!status.status.success());
    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(stderr.contains("repository deployment requires an embedded upgrade"));
    assert!(stderr.contains("aethyme upgrade plan"));
}

#[cfg(unix)]
#[test]
fn plan_refuses_managed_paths_that_escape_through_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tmp_dir();
    let repo = repository(temp.path());
    old_canonical_deployment(&repo);
    fs::remove_dir_all(repo.join(".codex")).unwrap();
    let outside = temp.path().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, repo.join(".codex")).unwrap();

    let plan = json(&run(&repo, &["upgrade", "plan", "--json"]));
    assert_eq!(plan["safe"], false);
    assert!(plan["blockers"].as_array().unwrap().iter().any(|blocker| {
        blocker
            .as_str()
            .unwrap()
            .contains("never write through symlinks")
    }));
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
}

#[test]
fn local_only_upgrade_stays_untracked_and_does_not_follow_the_clone() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    let bridge = run(
        &repo,
        &["deploy", "bridge", "--repo", repo.to_str().unwrap()],
    );
    assert!(bridge.status.success());
    commit_all(&repo, "add local activation bridge");
    let deployed = run(
        &repo,
        &["deploy", "--local-only", "--repo", repo.to_str().unwrap()],
    );
    assert!(deployed.status.success());
    fs::remove_file(repo.join(".aethyme/local/repository.json")).unwrap();
    assert_eq!(git_status(&repo), "");

    let plan = json(&run(&repo, &["upgrade", "plan", "--local-only", "--json"]));
    let digest = plan["plan_digest"].as_str().unwrap();
    let applied = run(
        &repo,
        &[
            "upgrade",
            "apply",
            "--local-only",
            "--confirm",
            digest,
            "--json",
        ],
    );
    assert_eq!(json(&applied)["applied"], true);
    assert!(repo.join(".aethyme/local/repository.json").is_file());
    assert_eq!(git_status(&repo), "");
}

fn git_status(repo: &Path) -> String {
    let output = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
