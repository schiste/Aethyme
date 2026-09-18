//! Host-scoped storage inventory and reviewed reclamation.

use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::Broker;

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

fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let repo = tempfile::tempdir().unwrap();
    let container = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);
    std::fs::create_dir_all(repo.path().join(".aethyme")).unwrap();
    std::fs::write(
        repo.path().join(".aethyme/broker.toml"),
        "[retention]\norphan_worktree_roots_days = 0\n",
    )
    .unwrap();
    let broker = Broker::open(repo.path()).unwrap();
    drop(broker);
    (repo, container)
}

fn enroll(repo: &Path) {
    std::fs::write(
        repo.join(".aethyme/config.toml"),
        "[promote]\nmode = \"auto\"\n",
    )
    .unwrap();
    std::fs::write(
        repo.join(".gitignore"),
        "/.aethyme/\n/target/\n/build/\n/dist/\n/node_modules/\n/.venv/\n",
    )
    .unwrap();
    git(repo, &["add", ".gitignore"]);
    git(repo, &["add", "-f", ".aethyme/config.toml"]);
    git(repo, &["commit", "-qm", "enroll fixture"]);
}

fn run(repo: &Path, container: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .env("AETHYME_WORKTREE_ROOT", container)
        .output()
        .unwrap()
}

fn marker(root: &Path, key: &str, repository_root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(
        root.join(".aethyme-worktree-root.json"),
        serde_json::json!({
            "schema_version": 1,
            "repository_key": key,
            "repository_root": repository_root,
        })
        .to_string(),
    )
    .unwrap();
}

fn json(output: Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn storage_plan_reconciles_disk_git_and_session_ledger_without_writing() {
    let (repo, container) = fixture();
    let owner_root = container.path().join("owner-key");
    marker(&owner_root, "owner-key", repo.path());
    let registered = owner_root.join("registered");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "registered",
            registered.to_str().unwrap(),
            "HEAD",
        ],
    );
    let registered_path = std::fs::canonicalize(&registered).unwrap();
    let mut broker = Broker::open(repo.path()).unwrap();
    broker.adopt(&registered, Some("ledger claim")).unwrap();
    std::fs::create_dir_all(owner_root.join("stray")).unwrap();
    std::fs::write(owner_root.join("stray/data"), "stray\n").unwrap();

    let orphan_root = container.path().join("orphan-key");
    let missing_owner = container.path().join("deleted-repository");
    marker(&orphan_root, "orphan-key", &missing_owner);
    std::fs::create_dir_all(orphan_root.join("old-session")).unwrap();
    std::fs::write(orphan_root.join("old-session/data"), "orphan\n").unwrap();

    let unmarked_root = container.path().join("unmarked-key");
    std::fs::create_dir_all(unmarked_root.join("do-not-touch")).unwrap();
    std::fs::write(unmarked_root.join("do-not-touch/data"), "protected\n").unwrap();

    let db = repo.path().join(".aethyme/broker.db");
    let db_before = std::fs::read(&db).unwrap();
    let plan = json(run(
        repo.path(),
        container.path(),
        &["storage", "plan", "--json"],
    ));
    assert_eq!(
        std::fs::read(&db).unwrap(),
        db_before,
        "storage plan must not mutate the owner ledger"
    );
    assert_eq!(plan["schema_version"], 2);
    assert_eq!(plan["summary"]["root_count"], 3);
    assert_eq!(plan["summary"]["owner_present_count"], 1);
    assert_eq!(plan["summary"]["owner_missing_count"], 1);
    assert_eq!(plan["summary"]["stray_directory_count"], 1);
    assert_eq!(plan["summary"]["orphan_root_count"], 1);
    assert_eq!(plan["summary"]["candidate_count"], 2);

    let owner = plan["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|root| root["repository_key"] == "owner-key")
        .unwrap();
    assert_eq!(owner["worktree_count"], 2);
    assert_eq!(owner["git_registered_count"], 1);
    assert_eq!(owner["ledger_claimed_count"], 1);
    let entries = owner["reconciliation"]["entries"].as_array().unwrap();
    let registered_entry = entries
        .iter()
        .find(|entry| entry["path"] == registered_path.to_str().unwrap())
        .unwrap();
    assert_eq!(
        registered_entry["missing_from"].as_array().unwrap().len(),
        0
    );
    let stray_entry = entries
        .iter()
        .find(|entry| {
            entry["path"]
                == owner_root
                    .join("stray")
                    .canonicalize()
                    .unwrap()
                    .to_str()
                    .unwrap()
        })
        .unwrap();
    assert_eq!(
        stray_entry["missing_from"],
        serde_json::json!(["git_registration", "session_ledger"])
    );
    assert_eq!(stray_entry["kind"], "stray_directory");

    let unmarked = plan["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|root| root["path"] == unmarked_root.canonicalize().unwrap().to_str().unwrap())
        .unwrap();
    assert_eq!(unmarked["marker_status"], "missing");
    assert!(
        plan["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|candidate| candidate["path"] != unmarked_root.to_str().unwrap())
    );
}

/// Make an artifact directory look settled.
///
/// The primary lane refuses anything still changing, because a primary
/// checkout has no session whose close would prove a build had finished. A
/// fixture built milliseconds ago is indistinguishable from a live build, so
/// it has to be aged before it can stand in for an abandoned one.
fn settle(path: &std::path::Path) {
    for entry in std::fs::read_dir(path).unwrap().flatten() {
        let status = Command::new("touch")
            .args(["-t", "202001010000"])
            .arg(entry.path())
            .status()
            .unwrap();
        assert!(status.success(), "touch failed for {:?}", entry.path());
    }
    let status = Command::new("touch")
        .args(["-t", "202001010000"])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "touch failed for {path:?}");
}

#[test]
fn storage_plan_reports_primary_artifacts_and_never_candidates_tracked_output() {
    let (repo, container) = fixture();
    enroll(repo.path());
    std::fs::create_dir_all(repo.path().join("target")).unwrap();
    std::fs::write(repo.path().join("target/output"), "target\n").unwrap();
    std::fs::create_dir_all(repo.path().join("build")).unwrap();
    std::fs::write(repo.path().join("build/output"), "build\n").unwrap();
    std::fs::create_dir_all(repo.path().join("dist")).unwrap();
    std::fs::write(repo.path().join("dist/tracked.js"), "tracked\n").unwrap();
    git(repo.path(), &["add", "-f", "dist/tracked.js"]);
    git(repo.path(), &["commit", "-qm", "add tracked dist output"]);

    // A build that is running leaves the checkout clean, because `target/` is
    // git-ignored. Candidacy therefore also requires the tree to have stopped
    // moving, so prove the live case first and only then age the fixture.
    let busy = json(run(repo.path(), container.path(), &["storage", "--json"]));
    assert_eq!(
        busy["summary"]["primary_candidate_count"], 0,
        "an artifact still being written to must never be a candidate"
    );

    settle(&repo.path().join("target"));
    settle(&repo.path().join("build"));
    settle(&repo.path().join("dist"));

    let plan = json(run(repo.path(), container.path(), &["storage", "--json"]));
    assert_eq!(plan["summary"]["primary_checkout_count"], 1);
    assert_eq!(plan["summary"]["primary_artifact_count"], 3);
    assert_eq!(plan["summary"]["primary_candidate_count"], 2);
    let checkout = &plan["primary_checkouts"][0];
    assert_eq!(checkout["clean"], true);
    let dist = checkout["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|artifact| artifact["name"] == "dist")
        .unwrap();
    assert_eq!(dist["tracked"], true);
    assert_eq!(dist["reclaimable"], false);
    assert!(dist["reason"].as_str().unwrap().contains("tracked"));
    assert!(
        plan["primary_candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|candidate| candidate["name"] != "dist")
    );

    let digest = plan["digest"].as_str().unwrap();
    let applied = json(run(
        repo.path(),
        container.path(),
        &["storage", "apply", "--confirm", digest, "--json"],
    ));
    assert_eq!(applied["complete"], true);
    assert_eq!(applied["applied"].as_array().unwrap().len(), 2);
    assert!(!repo.path().join("target").exists());
    assert!(!repo.path().join("build").exists());
    assert!(repo.path().join("dist/tracked.js").exists());
}

#[test]
fn storage_plan_refuses_a_dirty_primary_checkout_as_a_whole() {
    let (repo, container) = fixture();
    enroll(repo.path());
    std::fs::create_dir_all(repo.path().join("target")).unwrap();
    std::fs::write(repo.path().join("target/output"), "target\n").unwrap();
    std::fs::write(repo.path().join("notes.txt"), "human work\n").unwrap();

    let plan = json(run(
        repo.path(),
        container.path(),
        &["storage", "plan", "--json"],
    ));
    let checkout = &plan["primary_checkouts"][0];
    assert_eq!(checkout["clean"], false);
    assert!(
        checkout["dirty_paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| path == "notes.txt")
    );
    assert!(
        checkout["blockers"][0]
            .as_str()
            .unwrap()
            .contains("whole checkout")
    );
    let target = checkout["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|artifact| artifact["name"] == "target")
        .unwrap();
    assert_eq!(target["reclaimable"], false);
    assert_eq!(plan["primary_candidates"].as_array().unwrap().len(), 0);

    let digest = plan["digest"].as_str().unwrap();
    let applied = json(run(
        repo.path(),
        container.path(),
        &["storage", "apply", "--confirm", digest, "--json"],
    ));
    assert_eq!(applied["complete"], true);
    assert!(applied["applied"].as_array().unwrap().is_empty());
    assert!(repo.path().join("target/output").exists());
}

#[test]
fn storage_apply_removes_only_the_reviewed_orphan_and_stray_paths() {
    let (repo, container) = fixture();
    let owner_root = container.path().join("owner-key");
    marker(&owner_root, "owner-key", repo.path());
    let stray = owner_root.join("stray");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(stray.join("data"), "stray\n").unwrap();

    let orphan_root = container.path().join("orphan-key");
    let missing_owner = container.path().join("deleted-repository");
    marker(&orphan_root, "orphan-key", &missing_owner);
    std::fs::create_dir_all(orphan_root.join("old-session")).unwrap();
    std::fs::write(orphan_root.join("old-session/data"), "orphan\n").unwrap();

    let protected_root = container.path().join("protected-key");
    std::fs::create_dir_all(protected_root.join("stray")).unwrap();
    std::fs::write(protected_root.join("stray/data"), "protected\n").unwrap();

    let plan = json(run(repo.path(), container.path(), &["storage", "--json"]));
    let digest = plan["digest"].as_str().unwrap();
    assert_eq!(plan["summary"]["candidate_count"], 2);

    let apply = json(run(
        repo.path(),
        container.path(),
        &["storage", "apply", "--confirm", digest, "--json"],
    ));
    assert_eq!(apply["complete"], true);
    assert_eq!(apply["applied"].as_array().unwrap().len(), 2);
    assert!(!orphan_root.exists());
    assert!(!stray.exists());
    assert!(protected_root.exists());
    assert!(protected_root.join("stray/data").exists());
}

#[test]
fn storage_apply_refuses_a_plan_when_a_new_stray_appears() {
    let (repo, container) = fixture();
    let owner_root = container.path().join("owner-key");
    marker(&owner_root, "owner-key", repo.path());
    let plan = json(run(
        repo.path(),
        container.path(),
        &["storage", "plan", "--json"],
    ));
    let digest = plan["digest"].as_str().unwrap().to_owned();
    std::fs::create_dir_all(owner_root.join("new-stray")).unwrap();

    let output = run(
        repo.path(),
        container.path(),
        &["storage", "apply", "--confirm", &digest, "--json"],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no longer matches current state"));
    assert!(owner_root.join("new-stray").exists());
}

#[test]
fn storage_plan_does_not_follow_a_root_symlink_or_remove_it() {
    let (repo, container) = fixture();
    let target = container.path().join("outside");
    std::fs::create_dir_all(target.join("data")).unwrap();
    let link = container.path().join("linked-root");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let link_path = link
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap()
        .join(link.file_name().unwrap());

    let plan = json(run(repo.path(), container.path(), &["storage", "--json"]));
    let root = plan["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|root| root["path"] == link_path.to_str().unwrap())
        .unwrap();
    assert_eq!(root["filesystem_kind"], "symlink");
    assert!(plan["candidates"].as_array().unwrap().is_empty());
    assert!(link.exists());
    assert!(target.join("data").exists());
}

#[test]
fn storage_reconciliation_lists_git_or_ledger_paths_missing_on_disk() {
    let (repo, container) = fixture();
    let owner_root = container.path().join("owner-key");
    marker(&owner_root, "owner-key", repo.path());
    let registered = owner_root.join("registered");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "registered",
            registered.to_str().unwrap(),
            "HEAD",
        ],
    );
    let mut broker = Broker::open(repo.path()).unwrap();
    broker.adopt(&registered, Some("ledger claim")).unwrap();
    drop(broker);
    let registered_path = std::fs::canonicalize(&registered).unwrap();
    std::fs::remove_dir_all(&registered).unwrap();

    let plan = json(run(repo.path(), container.path(), &["storage", "--json"]));
    let entries = plan["roots"][0]["reconciliation"]["entries"]
        .as_array()
        .unwrap();
    let entry = entries
        .iter()
        .find(|entry| entry["path"] == registered_path.to_str().unwrap())
        .unwrap_or_else(|| panic!("registered path was absent from reconciliation: {plan}"));
    assert_eq!(entry["on_disk"], false);
    assert_eq!(entry["git_registered"], true);
    assert_eq!(entry["ledger_claimed"], true);
    assert_eq!(entry["missing_from"], serde_json::json!(["disk"]));
}
