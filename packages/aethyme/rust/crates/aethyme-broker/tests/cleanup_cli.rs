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
    let session = broker.start_worktree("cleanup CLI fixture", None).unwrap();
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
    let session = broker.start_worktree("cleanup confirmation", None).unwrap();
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
    let refusal = String::from_utf8_lossy(&apply.stderr).into_owned();
    assert!(
        refusal.contains("no longer matches current state")
            && refusal.contains("aethyme broker cleanup --all-cleaned"),
        "{refusal}"
    );
    assert!(
        !refusal.contains("expected"),
        "a freshly computed digest must not be offered as a value to confirm: {refusal}"
    );
    assert!(worktree.exists());
    assert!(worktree.join("followup.txt").exists());
}

/// A worktree whose removal died partway is deregistered but still on disk:
/// `git worktree remove` unlinks the administrative entry first, then walks
/// the tree, and an `ENOTEMPTY` on the way out leaves the files behind with no
/// gitdir to reach them (#165). Nothing about that changes what the session
/// did, so cleanup must still judge it -- and finish the removal.
#[test]
fn an_interrupted_worktree_removal_is_judged_and_completed_rather_than_refused() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);

    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.start_worktree("interrupted removal", None).unwrap();
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

    // Reproduce the stranded state exactly: the administrative entry is gone,
    // the checkout is not.
    let gitdir = std::fs::read_to_string(worktree.join(".git")).unwrap();
    let gitdir = std::path::PathBuf::from(gitdir.trim().strip_prefix("gitdir:").unwrap().trim());
    std::fs::remove_dir_all(&gitdir).unwrap();
    assert!(worktree.join("done.txt").exists());
    drop(broker);

    let plan = run(tmp.path(), &["cleanup", "--all-cleaned", "--json"]);
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    let item = &plan["plan"]["worktrees"][0];
    assert_eq!(
        item["disposition"], "eligible",
        "orphaned worktree judged {}: {}",
        item["disposition"], item["reason"]
    );

    let digest = plan["plan"]["digest"].as_str().unwrap().to_owned();
    let applied = run(
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
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let applied: serde_json::Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert!(
        applied["failures"].as_array().unwrap().is_empty(),
        "{:?}",
        applied["failures"]
    );
    assert!(!worktree.exists(), "stranded worktree was not removed");
}

/// A squash merge rewrites the SHA, so no ancestry check can see the work
/// land. The recorded representation is the only evidence that survives, and
/// cleanup has to consult it or the session is uncleanable forever (#164).
#[test]
fn a_squash_merged_session_is_eligible_on_its_recorded_representation() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin.git");
    let checkout = tmp.path().join("checkout");
    std::fs::create_dir_all(&checkout).unwrap();
    git(
        tmp.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(&checkout, &["init", "-q", "-b", "main"]);
    std::fs::write(checkout.join("README.md"), "fixture\n").unwrap();
    std::fs::write(checkout.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&checkout, &["add", "-A"]);
    git(&checkout, &["commit", "-qm", "init"]);
    git(
        &checkout,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&checkout, &["push", "-q", "-u", "origin", "main"]);
    // The representation scan reads the default branch from `origin/HEAD`,
    // which a real clone gets for free.
    git(&checkout, &["remote", "set-head", "origin", "--auto"]);
    let tmp_root = tmp;
    let tmp = Holder(checkout);

    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.start_worktree("squash merged", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("feature.txt"), "feature\n").unwrap();
    git(&worktree, &["add", "feature.txt"]);
    git(&worktree, &["commit", "-qm", "feature"]);
    let session_head = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&worktree)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();

    // The squash merge: same content on main, a commit the session never made.
    std::fs::write(tmp.path().join("feature.txt"), "feature\n").unwrap();
    git(tmp.path(), &["add", "feature.txt"]);
    git(tmp.path(), &["commit", "-qm", "feature (#1)"]);

    let scan = broker.scan_session_representation(session.id).unwrap();
    assert!(
        scan.search.represented(),
        "squashed content was not located on the default branch"
    );
    broker
        .record_session_representation(session.id, &scan.digest)
        .unwrap();
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
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    let item = &plan["plan"]["worktrees"][0];
    assert_eq!(
        item["disposition"], "eligible",
        "squash-merged session judged {}: {}",
        item["disposition"], item["reason"]
    );
    let provenance = &item["provenance"];
    assert_eq!(provenance["session_head"], session_head);
    assert!(
        provenance["represented_by_commit"].is_string(),
        "eligibility did not name the commit that carried the work: {provenance}"
    );
    drop(tmp_root);
}

/// Lets the squash fixture keep the `tmp.path()` spelling of its siblings
/// while working inside a checkout that has an `origin` beside it.
struct Holder(std::path::PathBuf);

impl Holder {
    fn path(&self) -> &Path {
        &self.0
    }
}
