use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{Broker, FinishOptions};

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

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

#[test]
fn bulk_cleanup_is_dry_run_by_default_and_apply_revalidates() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);

    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.start_worktree("cleanup CLI fixture").unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "done.txt"]);
    git(&worktree, &["commit", "-qm", "done"]);
    assert!(broker.submit(session.id).unwrap().promoted);
    assert!(
        broker
            .finish_with_options(
                session.id,
                FinishOptions {
                    keep_worktree: true,
                },
            )
            .unwrap()
            .closed
    );
    std::fs::create_dir_all(worktree.join("target/debug")).unwrap();
    std::fs::write(worktree.join("target/debug/cache.bin"), vec![3_u8; 2048]).unwrap();
    drop(broker);

    let plan = run(tmp.path(), &["cleanup", "--all-cleaned", "--json"]);
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(plan["applied"], false);
    assert_eq!(plan["plan"]["retained_worktree_count"], 1);
    assert_eq!(plan["plan"]["eligible_worktree_count"], 1);
    assert_eq!(plan["plan"]["schema_version"], 1);
    let digest = plan["plan"]["digest"].as_str().unwrap().to_owned();
    assert_eq!(digest.len(), 64);
    assert!(plan["removed_session_ids"].as_array().unwrap().is_empty());
    assert!(worktree.exists());

    let unconfirmed = run(
        tmp.path(),
        &["cleanup", "--all-cleaned", "--apply", "--json"],
    );
    assert!(!unconfirmed.status.success());
    assert!(worktree.exists());

    let apply = run(
        tmp.path(),
        &[
            "cleanup",
            "--all-cleaned",
            "--apply",
            "--confirm",
            &digest,
            "--json",
        ],
    );
    assert!(
        apply.status.success(),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    let apply: serde_json::Value = serde_json::from_slice(&apply.stdout).unwrap();
    assert_eq!(apply["applied"], true);
    assert_eq!(
        apply["removed_session_ids"],
        serde_json::json!([session.id])
    );
    assert!(apply["failures"].as_array().unwrap().is_empty());
    assert!(!worktree.exists());
    assert!(
        !Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{}", session.branch),
            ])
            .current_dir(tmp.path())
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn bulk_cleanup_rejects_force_and_session_mixups() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);

    for args in [
        vec!["cleanup", "--all-cleaned", "--force"],
        vec!["cleanup", "12", "--all-cleaned"],
        vec!["cleanup", "12", "--apply"],
    ] {
        let output = run(tmp.path(), &args);
        assert!(!output.status.success(), "unexpected success for {args:?}");
    }
}

#[test]
fn bulk_cleanup_confirmation_binds_the_exact_reviewed_branch_tip() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);

    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.start_worktree("cleanup confirmation").unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "done.txt"]);
    git(&worktree, &["commit", "-qm", "done"]);
    assert!(broker.submit(session.id).unwrap().promoted);
    assert!(
        broker
            .finish_with_options(
                session.id,
                FinishOptions {
                    keep_worktree: true,
                },
            )
            .unwrap()
            .closed
    );
    drop(broker);

    let plan = run(tmp.path(), &["cleanup", "--all-cleaned", "--json"]);
    assert!(plan.status.success());
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    let digest = plan["plan"]["digest"].as_str().unwrap().to_owned();

    std::fs::write(worktree.join("followup.txt"), "must survive\n").unwrap();
    git(&worktree, &["add", "followup.txt"]);
    git(&worktree, &["commit", "-qm", "pending followup"]);

    let apply = run(
        tmp.path(),
        &[
            "cleanup",
            "--all-cleaned",
            "--apply",
            "--confirm",
            &digest,
            "--json",
        ],
    );
    assert!(!apply.status.success());
    assert!(
        String::from_utf8_lossy(&apply.stderr).contains("confirmation mismatch"),
        "{}",
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(worktree.exists());
    assert!(worktree.join("followup.txt").exists());
}
