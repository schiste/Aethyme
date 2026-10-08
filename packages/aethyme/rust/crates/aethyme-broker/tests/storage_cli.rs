//! Host-scoped storage inventory and reviewed reclamation.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::Broker;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

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
    common::broker_cli(CLI, args)
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
    git(repo.path(), &["branch", "agent/unclaimed", "HEAD"]);
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
        &["gc", "storage", "plan", "--json"],
    ));
    assert_eq!(
        std::fs::read(&db).unwrap(),
        db_before,
        "storage plan must not mutate the owner ledger"
    );
    assert_eq!(plan["schema_version"], 3);
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
    assert_eq!(owner["session_branch_inventory_complete"], true);
    let session_branches = owner["session_branches"].as_array().unwrap();
    let registered_branch = session_branches
        .iter()
        .find(|branch| branch["name"] == "registered")
        .unwrap();
    assert_eq!(registered_branch["ownership"], "session_ledger");
    assert_eq!(
        registered_branch["session_ids"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        registered_branch["uncleared_session_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        registered_branch["checked_out_paths"],
        serde_json::json!([registered_path.clone()])
    );
    let unclaimed_branch = session_branches
        .iter()
        .find(|branch| branch["name"] == "agent/unclaimed")
        .unwrap();
    assert_eq!(unclaimed_branch["ownership"], "unknown");
    assert!(
        unclaimed_branch["session_ids"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        unclaimed_branch["retention_reason"]
            .as_str()
            .unwrap()
            .contains("ownership is unknown")
    );
    assert_eq!(plan["summary"]["broker_branch_ref_count"], 1);
    assert_eq!(plan["summary"]["unclaimed_agent_branch_ref_count"], 1);
    assert_eq!(plan["summary"]["incomplete_branch_inventory_root_count"], 0);
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
    let busy = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "--json"],
    ));
    assert_eq!(
        busy["summary"]["primary_candidate_count"], 0,
        "an artifact still being written to must never be a candidate"
    );

    settle(&repo.path().join("target"));
    settle(&repo.path().join("build"));
    settle(&repo.path().join("dist"));

    let plan = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "--json"],
    ));
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
        &["gc", "storage", "apply", "--confirm", digest, "--json"],
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
        &["gc", "storage", "plan", "--json"],
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
        &["gc", "storage", "apply", "--confirm", digest, "--json"],
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

    let plan = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "--json"],
    ));
    let digest = plan["digest"].as_str().unwrap();
    assert_eq!(plan["summary"]["candidate_count"], 2);

    let apply = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "apply", "--confirm", digest, "--json"],
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
        &["gc", "storage", "plan", "--json"],
    ));
    let digest = plan["digest"].as_str().unwrap().to_owned();
    std::fs::create_dir_all(owner_root.join("new-stray")).unwrap();

    let output = run(
        repo.path(),
        container.path(),
        &["gc", "storage", "apply", "--confirm", &digest, "--json"],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no longer matches current state"));
    assert!(owner_root.join("new-stray").exists());
}

#[test]
fn storage_apply_binds_preparation_candidates_to_confirmation() {
    let (repo, host) = fixture();
    let container = host.path().join("worktrees");
    std::fs::create_dir_all(&container).unwrap();
    let empty = json(run(
        repo.path(),
        &container,
        &["gc", "storage", "plan", "--json"],
    ));
    let entry = host.path().join("preparation-cache/repository/new-key");
    std::fs::create_dir_all(&entry).unwrap();
    std::fs::write(entry.join("payload"), "must survive stale approval").unwrap();
    let populated = json(run(
        repo.path(),
        &container,
        &["gc", "storage", "plan", "--json"],
    ));
    assert_ne!(empty["digest"], populated["digest"]);
    assert_eq!(populated["summary"]["preparation_candidate_count"], 1);

    let refused = run(
        repo.path(),
        &container,
        &[
            "gc",
            "storage",
            "apply",
            "--confirm",
            empty["digest"].as_str().unwrap(),
            "--json",
        ],
    );
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("no longer matches current state"));
    assert!(entry.join("payload").is_file());

    let applied = json(run(
        repo.path(),
        &container,
        &[
            "gc",
            "storage",
            "apply",
            "--confirm",
            populated["digest"].as_str().unwrap(),
            "--json",
        ],
    ));
    assert_eq!(applied["complete"], true);
    assert!(!entry.exists());
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

    let plan = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "--json"],
    ));
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

    let plan = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "--json"],
    ));
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

/// `git worktree add` aimed one level too high leaves a checkout where a
/// worktree root belongs. Reconciled as a root, its source directories are
/// unregistered, unclaimed children -- strays -- and any ownership marker
/// beside them would authorize deleting them.
#[test]
fn a_worktree_placed_at_the_root_level_is_never_reconciled_as_a_root() {
    let (repo, container) = fixture();
    let checkout = container.path().join("repo-feature");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            checkout.to_str().unwrap(),
        ],
    );
    std::fs::create_dir_all(checkout.join("docs")).unwrap();
    std::fs::write(checkout.join("docs/guide.md"), "unpublished\n").unwrap();
    std::fs::create_dir_all(checkout.join("src")).unwrap();
    std::fs::write(checkout.join("src/lib.rs"), "// unpublished\n").unwrap();
    // The worst case: something wrote an ownership marker into the checkout.
    marker(&checkout, "repo-feature", repo.path());

    let plan = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "--json"],
    ));
    let root = plan["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|root| root["path"].as_str().unwrap().ends_with("repo-feature"))
        .unwrap();
    assert!(
        root["reconciliation"]["entries"]
            .as_array()
            .unwrap()
            .is_empty(),
        "a checkout's contents must not be reconciled: {root:#}"
    );
    let blockers = root["blockers"].as_array().unwrap();
    assert!(
        blockers.iter().any(|blocker| {
            let text = blocker.as_str().unwrap();
            text.contains("placed directly in the worktree container") && text.contains("feature")
        }),
        "{blockers:?}"
    );
    assert_eq!(plan["summary"]["candidate_count"], 0, "{plan:#}");
    assert!(checkout.join("docs/guide.md").exists());
    assert!(checkout.join("src/lib.rs").exists());
}

/// Start a session through the CLI and finish it; returns its worktree.
fn start_and_finish(repo: &Path, container: &Path, task: &str, finish: &[&str]) -> PathBuf {
    let started = run(repo, container, &["start", "--task", task, "--json"]);
    let session = json(started);
    let worktree = PathBuf::from(session["worktree_path"].as_str().unwrap());
    let id = session["id"].as_i64().unwrap().to_string();
    let mut args = vec!["finish", "--session", id.as_str()];
    args.extend_from_slice(finish);
    let finished = run(repo, container, &args);
    assert!(
        finished.status.success(),
        "finish: {}",
        String::from_utf8_lossy(&finished.stderr)
    );
    worktree
}

/// The only root in the fixture container that belongs to the fixture repo.
fn own_root(plan: &serde_json::Value) -> serde_json::Value {
    let roots = plan["roots"].as_array().unwrap();
    let owned = roots
        .iter()
        .filter(|root| root["owner_exists"] == true)
        .collect::<Vec<_>>();
    assert_eq!(owned.len(), 1, "{plan}");
    owned[0].clone()
}

/// map-coloring and map-generator: every session finished and cleaned up, no
/// worktree left. Each finished session's ledger row used to be reported as
/// a worktree missing from disk and Git.
#[test]
fn worktrees_removed_by_finished_sessions_are_retired_not_missing() {
    let (repo, container) = fixture();
    for task in ["first finished task", "second finished task"] {
        let worktree = start_and_finish(repo.path(), container.path(), task, &[]);
        assert!(!worktree.exists(), "finish should remove {worktree:?}");
    }

    let plan = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "--json"],
    ));
    let root = own_root(&plan);
    assert_eq!(root["retired_count"], 2, "{root}");
    assert_eq!(root["ledger_claimed_count"], 0, "{root}");
    assert_eq!(root["reconciliation"]["retired_count"], 2, "{root}");
    assert!(
        root["reconciliation"]["entries"]
            .as_array()
            .unwrap()
            .is_empty(),
        "retired worktrees are not drift: {root}"
    );

    let detail = run(
        repo.path(),
        container.path(),
        &["gc", "storage", "--detail"],
    );
    let text = String::from_utf8_lossy(&detail.stdout);
    assert!(!text.contains("drift:"), "{text}");
    assert!(text.contains("retired: 2 worktree(s)"), "{text}");
}

/// A session that kept its worktree has not retired it: if that checkout
/// disappears, something other than cleanup removed it.
#[test]
fn a_kept_worktree_that_disappears_is_still_reported_missing() {
    let (repo, container) = fixture();
    let worktree = start_and_finish(
        repo.path(),
        container.path(),
        "kept worktree",
        &["--keep-worktree"],
    );
    let canonical = std::fs::canonicalize(&worktree).unwrap();
    git(
        repo.path(),
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );

    let plan = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "--json"],
    ));
    let root = own_root(&plan);
    assert_eq!(root["retired_count"], 0, "{root}");
    let entry = root["reconciliation"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == canonical.to_str().unwrap())
        .cloned()
        .unwrap_or_else(|| panic!("kept worktree was not reported: {root}"));
    assert_eq!(
        entry["missing_from"],
        serde_json::json!(["disk", "git_registration"])
    );
    assert!(
        plan["candidates"].as_array().unwrap().is_empty(),
        "a root a session still claims is never empty: {plan}"
    );
}

/// The last worktree is gone and only bookkeeping remains: the root becomes a
/// reviewed candidate, and the next `broker start` recreates it.
#[test]
fn an_empty_root_is_a_reviewed_candidate_and_the_next_start_recreates_it() {
    let (repo, container) = fixture();
    start_and_finish(repo.path(), container.path(), "only task", &[]);
    let plan = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "--json"],
    ));
    let root = own_root(&plan);
    assert_eq!(root["empty"], true, "{root}");
    let root_path = PathBuf::from(root["path"].as_str().unwrap());
    assert!(root_path.join(".cargo/config.toml").is_file());
    let candidates = plan["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1, "{plan}");
    assert_eq!(candidates[0]["kind"], "empty_root");
    assert_eq!(plan["summary"]["empty_root_count"], 1);

    let digest = plan["digest"].as_str().unwrap();
    let applied = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "apply", "--confirm", digest, "--json"],
    ));
    assert_eq!(applied["complete"], true, "{applied}");
    assert!(!root_path.exists(), "the empty root should be removed");

    let started = json(run(
        repo.path(),
        container.path(),
        &["start", "--task", "after the sweep", "--json"],
    ));
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    assert!(worktree.is_dir(), "start must recreate the root: {started}");
    assert!(root_path.join(".aethyme-worktree-root.json").is_file());
    assert!(root_path.join(".cargo/config.toml").is_file());
}

/// Anything the broker did not write there -- including build defaults an
/// operator edited -- is content no plan judged, so the root is not empty.
#[test]
fn a_root_holding_anything_but_bookkeeping_is_not_empty() {
    for extra in ["notes", "edited-cargo-config"] {
        let (repo, container) = fixture();
        start_and_finish(repo.path(), container.path(), "only task", &[]);
        let root = own_root(&json(run(
            repo.path(),
            container.path(),
            &["gc", "storage", "--json"],
        )));
        let root_path = PathBuf::from(root["path"].as_str().unwrap());
        match extra {
            "notes" => std::fs::write(root_path.join(".notes"), "mine\n").unwrap(),
            _ => std::fs::write(root_path.join(".cargo/config.toml"), "[build]\n").unwrap(),
        }

        let plan = json(run(
            repo.path(),
            container.path(),
            &["gc", "storage", "--json"],
        ));
        assert_eq!(own_root(&plan)["empty"], false, "{extra}: {plan}");
        assert!(
            plan["candidates"].as_array().unwrap().is_empty(),
            "{extra}: {plan}"
        );
        assert!(root_path.exists());
    }
}

/// Recovery archives beside the worktree container are listed so they are
/// never invisible, and a hand-made kit is told apart from a broker archive.
/// Listing them authorizes nothing: the digest ignores them.
#[test]
fn storage_plan_lists_recovery_archives_without_proposing_them() {
    let (repo, _unused) = fixture();
    let host = tempfile::tempdir().unwrap();
    let container = host.path().join("worktrees");
    std::fs::create_dir_all(&container).unwrap();
    let before = json(run(
        repo.path(),
        &container,
        &["gc", "storage", "plan", "--json"],
    ));

    let archives = host.path().join("recovery-archives");
    let broker_archive = archives.join("owner-key/12-0123456789ab-1790000000000");
    std::fs::create_dir_all(&broker_archive).unwrap();
    std::fs::write(
        broker_archive.join("manifest.json"),
        serde_json::json!({
            "schema_version": 1,
            "created_at_ms": 1_790_000_000_000_i64,
            "session_id": 12,
            "head": "0123456789abcdef0123456789abcdef01234567",
        })
        .to_string(),
    )
    .unwrap();
    let manual = archives.join("mockup-temporal/20260921T071003Z");
    std::fs::create_dir_all(&manual).unwrap();
    std::fs::write(manual.join("manifest.json"), r#"{"worktrees": []}"#).unwrap();

    let plan = json(run(
        repo.path(),
        &container,
        &["gc", "storage", "plan", "--json"],
    ));
    let groups = plan["recovery_archives"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "{plan}");
    let group = |suffix: &str| {
        groups
            .iter()
            .find(|group| group["path"].as_str().unwrap().ends_with(suffix))
            .unwrap()
    };
    assert_eq!(group("mockup-temporal")["archive_count"], 1);
    assert_eq!(group("mockup-temporal")["broker_archive_count"], 0);
    assert_eq!(group("owner-key")["broker_archive_count"], 1);
    assert_eq!(
        group("owner-key")["oldest_created_at_ms"],
        1_790_000_000_000_i64
    );
    assert_eq!(
        plan["digest"], before["digest"],
        "listing authorizes nothing"
    );
    assert!(broker_archive.exists() && manual.exists());
}

fn root_entry(plan: &serde_json::Value, root: &Path) -> serde_json::Value {
    let root = root.canonicalize().unwrap();
    plan["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == root.to_str().unwrap())
        .cloned()
        .unwrap_or_else(|| panic!("no root {} in {plan}", root.display()))
}

/// A root created before markers existed holds this repository's worktrees,
/// but nothing could ever reclaim its strays, because nothing could say who
/// owned it (#257). Attribution proves ownership from Git's own registrations,
/// reports before it writes, and once recorded the root is reconciled like any
/// other.
#[test]
fn a_root_whose_worktrees_are_registered_here_is_attributed_then_reconciled() {
    let (repo, container) = fixture();
    let old_root = container.path().join("renamed-checkout-0123456789abcdef");
    std::fs::create_dir_all(&old_root).unwrap();
    let worktree = old_root.join("registered");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "registered",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let stray = old_root.join("stray");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(stray.join("data"), "stray\n").unwrap();
    let marker_path = old_root.join(".aethyme-worktree-root.json");

    let plan = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "plan", "--json"],
    ));
    let root = root_entry(&plan, &old_root);
    assert_eq!(root["marker_status"], "missing");
    assert!(
        root["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker
                .as_str()
                .unwrap()
                .contains("storage attribute --apply")),
        "{root}"
    );
    assert_eq!(plan["summary"]["candidate_count"], 0);

    let report = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "attribute", "--json"],
    ));
    assert_eq!(report["applied"], false);
    let entry = &report["roots"][0];
    assert_eq!(entry["attributable"], true, "{report}");
    assert_eq!(entry["marked"], false);
    assert_eq!(entry["registered_worktree_count"], 1);
    assert!(!marker_path.exists(), "a report writes nothing");

    let applied = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "attribute", "--apply", "--json"],
    ));
    assert_eq!(applied["roots"][0]["marked"], true, "{applied}");
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
    assert_eq!(
        marker["repository_root"],
        repo.path().canonicalize().unwrap().to_str().unwrap()
    );

    let plan = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "plan", "--json"],
    ));
    assert_eq!(root_entry(&plan, &old_root)["marker_status"], "valid");
    let candidates = plan["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1, "{plan}");
    assert_eq!(candidates[0]["kind"], "stray_directory");
    assert_eq!(
        candidates[0]["path"],
        stray.canonicalize().unwrap().to_str().unwrap()
    );
    assert!(
        worktree.exists() && stray.exists(),
        "planning removes nothing"
    );
}

/// Ownership is never inferred from weaker evidence: a root holding a
/// worktree another repository registered, or no worktree under a foreign
/// name, stays unmarked and is reported with the reason.
#[test]
fn a_root_this_repository_cannot_prove_is_never_marked() {
    let (repo, container) = fixture();
    let other = tempfile::tempdir().unwrap();
    git(other.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(other.path().join("README.md"), "other\n").unwrap();
    git(other.path(), &["add", "-A"]);
    git(other.path(), &["commit", "-qm", "init"]);
    let foreign_root = container.path().join("other-repository-key");
    std::fs::create_dir_all(&foreign_root).unwrap();
    let foreign_worktree = foreign_root.join("theirs");
    git(
        other.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "theirs",
            foreign_worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let bare_root = container.path().join("nobody-key");
    std::fs::create_dir_all(bare_root.join("files")).unwrap();

    let applied = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "attribute", "--apply", "--json"],
    ));
    let roots = applied["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 2, "{applied}");
    for root in roots {
        assert_eq!(root["attributable"], false, "{root}");
        assert_eq!(root["marked"], false);
    }
    assert!(roots.iter().any(|root| {
        root["reason"]
            .as_str()
            .unwrap()
            .contains("not registered in this repository")
    }));
    assert!(!foreign_root.join(".aethyme-worktree-root.json").exists());
    assert!(!bare_root.join(".aethyme-worktree-root.json").exists());

    let plan = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "plan", "--json"],
    ));
    assert!(
        root_entry(&plan, &foreign_root)["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker
                .as_str()
                .unwrap()
                .contains("not attributable to this repository"))
    );
    assert_eq!(plan["summary"]["candidate_count"], 0);
}

#[test]
fn a_shared_branch_named_by_a_session_is_reported_shared_not_session_owned() {
    let (repo, container) = fixture();
    let owner_root = container.path().join("owner-key");
    marker(&owner_root, "owner-key", repo.path());
    // A ledger row naming the integration branch, as an old adoption of a
    // checkout on a shared branch leaves behind.
    let shared = owner_root.join("shared");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "aethyme/integration",
            shared.to_str().unwrap(),
            "HEAD",
        ],
    );
    let mut broker = Broker::open(repo.path()).unwrap();
    broker.adopt(&shared, Some("shared branch claim")).unwrap();

    let plan = json(run(
        repo.path(),
        container.path(),
        &["gc", "storage", "plan", "--json"],
    ));
    let owner = own_root(&plan);
    let branches = owner["session_branches"].as_array().unwrap();
    let integration = branches
        .iter()
        .find(|branch| branch["name"] == "aethyme/integration")
        .expect("the ledger-named shared branch is listed");
    assert_eq!(integration["ownership"], "shared");
    assert!(
        integration["retention_reason"]
            .as_str()
            .unwrap()
            .contains("never session-owned or removable")
    );
    assert_eq!(plan["summary"]["broker_branch_ref_count"], 0);
}
