use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{BrokerStore, GitRepo, NewSession, SessionOrigin};
use aethyme_testkit::{aethyme_bin, tmp_dir};
use serde_json::Value;

fn initialized_repository(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    fs::create_dir(&repo).unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-b", "main"])
            .arg(&repo)
            .output()
            .unwrap()
            .status
            .success()
    );
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"sample\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::create_dir(repo.join("src")).unwrap();
    fs::write(repo.join("src/main.rs"), "fn main() {}\n").unwrap();
    commit_all(&repo, "initial");
    repo
}

fn repository(root: &Path) -> PathBuf {
    let repo = initialized_repository(root);
    let deployed = run(&repo, &["deploy", "--repo", repo.to_str().unwrap()]);
    assert_success(&deployed);
    commit_all(&repo, "deploy");
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

fn run(repo: &Path, args: &[&str]) -> Output {
    command(repo).args(args).output().unwrap()
}

fn commit_all(repo: &Path, message: &str) {
    assert!(
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(repo)
            .output()
            .unwrap()
            .status
            .success()
    );
    let output = Command::new("git")
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
    assert_success(&output);
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn json(output: &Output) -> Value {
    assert_success(output);
    serde_json::from_slice(&output.stdout).unwrap()
}

fn remove_for_remediation(repo: &Path) {
    for relative in [
        ".aethyme/config.toml",
        ".aethyme/gates.toml",
        ".aethyme/generated/onboarding.json",
        "AGENTS.md",
        "CLAUDE.md",
    ] {
        fs::remove_file(repo.join(relative)).unwrap();
    }
    commit_all(repo, "remove managed readiness artifacts");
}

#[test]
fn plan_is_read_only_complete_and_digest_bound() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    remove_for_remediation(&repo);
    let before = git_status(&repo);

    let plan = json(&run(&repo, &["broker", "readiness", "plan", "--json"]));

    assert_eq!(plan["schema_version"], 1);
    assert_eq!(plan["repository_mode"], "canonical");
    assert_eq!(plan["repository_schema"], 1);
    assert_eq!(plan["source_head"].as_str().unwrap().len(), 40);
    assert_eq!(plan["managed_state_digest"].as_str().unwrap().len(), 64);
    assert_eq!(plan["plan_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(plan["safe"], true, "{plan:#}");
    for path in [
        ".aethyme/config.toml",
        ".aethyme/gates.toml",
        ".aethyme/generated/onboarding.json",
        "AGENTS.md",
    ] {
        assert!(
            plan["planned_write_set"]
                .as_array()
                .unwrap()
                .contains(&Value::String(path.into()))
        );
    }
    assert!(
        plan["actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| { action["kind"] == "gates_draft" && action["review_required"] == true })
    );
    assert_eq!(git_status(&repo), before);
}

#[test]
fn reviewed_diff_and_confirmed_apply_share_exact_outputs() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    remove_for_remediation(&repo);
    let plan = json(&run(&repo, &["broker", "readiness", "plan", "--json"]));
    let digest = plan["plan_sha256"].as_str().unwrap();
    let diff = run(&repo, &["broker", "readiness", "plan", "--diff"]);
    assert_success(&diff);
    let diff = String::from_utf8(diff.stdout).unwrap();
    assert!(diff.contains("Remediation diff:"));
    assert!(diff.contains("diff --git a/AGENTS.md b/AGENTS.md"));

    let applied = json(&run(
        &repo,
        &[
            "broker",
            "readiness",
            "apply",
            "--confirm",
            digest,
            "--json",
        ],
    ));
    assert_eq!(applied["applied"], true);
    assert_eq!(applied["plan_sha256"], digest);
    assert!(repo.join("AGENTS.md").is_file());
    assert!(repo.join(".aethyme/config.toml").is_file());
    assert!(
        repo.join(".aethyme/generated/experience-status.json")
            .is_file()
    );
    assert!(
        fs::read_to_string(repo.join(".aethyme/gates.toml"))
            .unwrap()
            .contains("reviewed = false")
    );

    let readiness = json(&run(&repo, &["broker", "readiness", "--json"]));
    let validation = readiness["dimensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|dimension| dimension["id"] == "validation")
        .unwrap();
    assert_eq!(validation["state"], "limited");
    assert!(
        validation["summary"]
            .as_str()
            .unwrap()
            .contains("unreviewed")
    );
    let gates_path = repo.join(".aethyme/gates.toml");
    let reviewed = fs::read_to_string(&gates_path)
        .unwrap()
        .replace("reviewed = false", "reviewed = true");
    fs::write(&gates_path, reviewed).unwrap();
    let readiness = json(&run(&repo, &["broker", "readiness", "--json"]));
    let validation = readiness["dimensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|dimension| dimension["id"] == "validation")
        .unwrap();
    assert!(
        !validation["summary"]
            .as_str()
            .unwrap()
            .contains("unreviewed")
    );
}

#[test]
fn confirmation_and_head_drift_are_refused_without_writes() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    remove_for_remediation(&repo);
    let plan = json(&run(&repo, &["broker", "readiness", "plan", "--json"]));
    let digest = plan["plan_sha256"].as_str().unwrap().to_string();
    fs::write(repo.join("unrelated.txt"), "committed later\n").unwrap();
    commit_all(&repo, "move head");

    let refused = run(
        &repo,
        &["broker", "readiness", "apply", "--confirm", &digest],
    );
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("changed after review"));
    assert!(!repo.join("AGENTS.md").exists());
}

#[test]
fn customized_policy_and_symlink_targets_are_never_replaced() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    fs::write(repo.join("AGENTS.md"), "maintainer policy\n").unwrap();
    commit_all(&repo, "custom policy");
    let customized = json(&run(&repo, &["broker", "readiness", "plan", "--json"]));
    assert_eq!(customized["safe"], false);
    assert!(
        customized["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| {
                item.as_str()
                    .unwrap()
                    .contains("customized policy AGENTS.md")
            })
    );
    assert_eq!(
        fs::read_to_string(repo.join("AGENTS.md")).unwrap(),
        "maintainer policy\n"
    );

    fs::remove_file(repo.join("AGENTS.md")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("Cargo.toml", repo.join("AGENTS.md")).unwrap();
    commit_all(&repo, "symlink policy");
    let symlinked = json(&run(&repo, &["broker", "readiness", "plan", "--json"]));
    assert_eq!(symlinked["safe"], false);
    assert!(
        symlinked["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item.as_str().unwrap().contains("crosses symlink") })
    );
}

#[test]
fn interrupted_apply_is_recovered_only_by_the_exact_plan_digest() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    remove_for_remediation(&repo);
    let before = git_status(&repo);
    let plan = json(&run(&repo, &["broker", "readiness", "plan", "--json"]));
    let digest = plan["plan_sha256"].as_str().unwrap().to_string();

    let crashed = command(&repo)
        .env("AETHYME_TEST_UPGRADE_CRASH", "after_first_replacement")
        .args(["broker", "readiness", "apply", "--confirm", &digest])
        .output()
        .unwrap();
    assert_eq!(crashed.status.code(), Some(86));

    let wrong = run(
        &repo,
        &["broker", "readiness", "recover", "--plan", &"0".repeat(64)],
    );
    assert!(!wrong.status.success());
    let recovered = json(&run(
        &repo,
        &[
            "broker",
            "readiness",
            "recover",
            "--plan",
            &digest,
            "--json",
        ],
    ));
    assert_eq!(recovered["recovered"], true);
    assert_eq!(recovered["plan_digest"], digest);
    assert_eq!(git_status(&repo), before);
}

#[test]
fn disjoint_work_is_preserved_and_overlapping_work_blocks() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    remove_for_remediation(&repo);
    fs::write(repo.join("notes.txt"), "keep me\n").unwrap();
    let disjoint = json(&run(&repo, &["broker", "readiness", "plan", "--json"]));
    assert_eq!(disjoint["safe"], true);
    assert_eq!(
        disjoint["dirty_disjoint_paths"],
        serde_json::json!(["notes.txt"])
    );
    assert!(disjoint.to_string().contains("not inputs"));
    assert!(!disjoint.to_string().contains("stash"));

    fs::write(repo.join("AGENTS.md"), "uncommitted overlap\n").unwrap();
    let overlapping = json(&run(&repo, &["broker", "readiness", "plan", "--json"]));
    assert_eq!(overlapping["safe"], false);
    assert!(
        overlapping["dirty_overlapping_paths"]
            .as_array()
            .unwrap()
            .contains(&Value::String("AGENTS.md".into()))
    );
    assert!(!overlapping.to_string().contains("stash"));
}

#[test]
fn live_sessions_and_exact_leases_block_the_reviewed_write_set() {
    let temp = tmp_dir();
    let repo = repository(temp.path());
    remove_for_remediation(&repo);
    let checkout = GitRepo::discover(&repo).unwrap();
    let head = git_stdout(&repo, &["rev-parse", "HEAD"]);
    let branch = git_stdout(&repo, &["branch", "--show-current"]);
    let mut store = BrokerStore::open_in_repo(&repo).unwrap();
    let session = store
        .register_session(&NewSession {
            worktree_path: checkout.root().to_string_lossy().into_owned(),
            branch,
            origin: SessionOrigin::Adopted,
            task: Some("hold an exact remediation path".into()),
            diff_base: Some(head.clone()),
            adoption_base: Some(head),
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
            agent_identity: None,
        })
        .unwrap();
    store.claim_lease(session.id, "AGENTS.md", None).unwrap();

    let plan = json(&run(&repo, &["broker", "readiness", "plan", "--json"]));
    assert_eq!(plan["safe"], false);
    assert_eq!(plan["live_sessions"][0]["session_id"], session.id);
    assert_eq!(plan["relevant_leases"][0]["path"], "AGENTS.md");
    assert!(plan["blockers"].as_array().unwrap().iter().any(|blocker| {
        blocker
            .as_str()
            .unwrap()
            .contains("overlap live session leases")
    }));
}

#[test]
fn local_only_remediation_stays_clone_local() {
    let temp = tmp_dir();
    let repo = initialized_repository(temp.path());
    let bridge = run(
        &repo,
        &["deploy", "bridge", "--repo", repo.to_str().unwrap()],
    );
    assert_success(&bridge);
    commit_all(&repo, "install inert local bridge");
    let deployed = run(
        &repo,
        &["deploy", "--local-only", "--repo", repo.to_str().unwrap()],
    );
    assert_success(&deployed);
    fs::remove_file(repo.join(".aethyme/local/AGENTS.md")).unwrap();

    let plan = json(&run(
        &repo,
        &["broker", "readiness", "plan", "--local-only", "--json"],
    ));
    assert_eq!(plan["repository_mode"], "local_only");
    assert_eq!(plan["safe"], true);
    assert!(
        plan["planned_write_set"]
            .as_array()
            .unwrap()
            .contains(&Value::String(".aethyme/local/AGENTS.md".into()))
    );
    let digest = plan["plan_sha256"].as_str().unwrap();
    let applied = json(&run(
        &repo,
        &[
            "broker",
            "readiness",
            "apply",
            "--local-only",
            "--confirm",
            digest,
            "--json",
        ],
    ));
    assert_eq!(applied["applied"], true);
    assert!(repo.join(".aethyme/local/AGENTS.md").is_file());
    assert_eq!(git_status(&repo), "");
}

fn git_status(repo: &Path) -> String {
    let output = Command::new("git")
        .args(["status", "--short"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert_success(&output);
    String::from_utf8(output.stdout).unwrap()
}

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert_success(&output);
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}
