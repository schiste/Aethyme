//! Synthetic, fully owned repository. No gate execution or provider calls.
use aethyme_broker::{
    GateCacheProvenance, GateEnvironment, GateFailureClass, GateStatus, NewGateResult, NewSession,
    SessionOrigin,
};
use std::process::{Command, Output};
mod common;
const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn isolate(command: &mut Command, fixture: &common::FixtureRepository) {
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command
        .env("HOME", fixture.owner_path().join("home"))
        .env(
            "GIT_CONFIG_GLOBAL",
            fixture.owner_path().join("empty-global"),
        )
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com");
}

fn git(fixture: &common::FixtureRepository, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command.current_dir(fixture.path()).args(args);
    isolate(&mut command, fixture);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn cli(fixture: &common::FixtureRepository, args: &[&str]) -> Output {
    let mut command = common::broker_cli_for_fixture(CLI, args, fixture.path());
    command.current_dir(fixture.path());
    isolate(&mut command, fixture);
    command.output().unwrap()
}

fn json(output: Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn fixture() -> (common::FixtureRepository, i64, i64) {
    let root = common::FixtureRepository::new();
    std::fs::create_dir_all(root.owner_path().join("home")).unwrap();
    std::fs::write(root.owner_path().join("empty-global"), "").unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "maintenance.auto", "false"]);
    git(&root, &["config", "gc.auto", "0"]);
    std::fs::create_dir_all(root.path().join(".aethyme")).unwrap();
    std::fs::write(
        root.path().join(".aethyme/config.toml"),
        "[cleanup]\nauto_remove=false\n[graph]\nauthority='disabled'\n",
    )
    .unwrap();
    let gates = "[[gate]]\nname='unit'\ncommand='exit 1'\n";
    std::fs::write(root.path().join(".aethyme/gates.toml"), gates).unwrap();
    std::fs::write(root.path().join("tracked.txt"), "base\n").unwrap();
    git(
        &root,
        &[
            "add",
            ".aethyme/gates.toml",
            ".aethyme/config.toml",
            "tracked.txt",
        ],
    );
    git(&root, &["commit", "-qm", "fixture"]);
    let base = git(&root, &["rev-parse", "HEAD"]);
    let tree = git(&root, &["rev-parse", "HEAD^{tree}"]);
    git(&root, &["update-ref", "refs/remotes/origin/main", &base]);
    git(
        &root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let mut broker = common::open_fixture_repository_broker(&root).unwrap();
    let session = broker
        .store()
        .register_session(&NewSession {
            worktree_path: root.path().to_string_lossy().into(),
            branch: "main".into(),
            origin: SessionOrigin::Adopted,
            task: Some("repair known base failure".into()),
            diff_base: Some(base),
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
            agent_identity: None,
        })
        .unwrap();
    let failed = broker
        .store()
        .record_gate_result_with_execution_profile(
            &NewGateResult {
                gate_name: "unit".into(),
                tree_hash: tree,
                definition_hash: aethyme_broker::parse_gates(gates).unwrap()[0]
                    .definition_hash
                    .clone(),
                status: GateStatus::Fail,
                failure_class: Some(GateFailureClass::TestFailure),
                exit_code: Some(1),
                duration_ms: Some(1),
                wait_duration_ms: None,
                first_output_ms: None,
                output_bytes: None,
                log_path: None,
                session_id: Some(session.id),
            },
            &GateEnvironment::default(),
            Some(&GateCacheProvenance {
                // Exact encoding emitted by gate_profile::digest, not bare hex.
                execution_profile_digest: Some(format!("sha256:{}", "c".repeat(64))),
                profile_scope: "synthetic fixture".into(),
            }),
        )
        .unwrap();
    drop(broker);
    (root, session.id, failed)
}

#[test]
fn cli_confirms_lists_and_status_reports_real_profile_encoded_base_failure() {
    let (root, owner, run) = fixture();
    let first = json(cli(
        &root,
        &[
            "gates",
            "incidents",
            "confirm",
            &run.to_string(),
            "unit::broken",
            "--session",
            &owner.to_string(),
            "--pr",
            "17",
            "--json",
        ],
    ));
    assert_eq!(first[0]["owner_session_id"], owner);
    assert_eq!(first[0]["repair_pr"], 17);
    assert_eq!(first[0]["test_ids"], serde_json::json!(["unit::broken"]));
    assert_eq!(
        first[0]["execution_profile_hash"],
        format!("sha256:{}", "c".repeat(64))
    );
    let list = json(cli(&root, &["gates", "incidents", "list", "--json"]));
    assert_eq!(list, first);
    let status = json(cli(&root, &["status", "--summary", "--json"]));
    let advice = status["advice"].as_array().unwrap();
    let incident = advice
        .iter()
        .find(|item| item["id"] == "base.failure-incident")
        .unwrap();
    assert_eq!(incident["session_id"], owner);
    assert_eq!(incident["reason"], "recorded_failure_on_fetched_base");
    assert!(
        incident["summary"]
            .as_str()
            .unwrap()
            .contains("Gates still run")
    );
    // A future fetched base must not be silently labeled with the old failure.
    std::fs::write(root.path().join("tracked.txt"), "next base\n").unwrap();
    git(&root, &["add", "tracked.txt"]);
    git(&root, &["commit", "-qm", "new base"]);
    let next = git(&root, &["rev-parse", "HEAD"]);
    git(&root, &["update-ref", "refs/remotes/origin/main", &next]);
    let status = json(cli(&root, &["status", "--summary", "--json"]));
    assert!(
        status["advice"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["reason"] == "prior_base_failure_not_requalified")
    );
    assert_eq!(
        json(cli(&root, &["gates", "incidents", "list", "--json"])),
        first
    );
}

#[test]
fn malformed_incident_commands_are_refused_before_broker_state_is_opened() {
    let root = common::FixtureRepository::new();
    for args in [
        vec![
            "gates",
            "incidents",
            "confirm",
            "0",
            "unit::bad",
            "--session",
            "1",
        ],
        vec!["gates", "incidents", "resolve", "1", "-2"],
        vec!["gates", "incidents", "list", "unexpected"],
        vec!["gates", "incidents", "list", "--session", "1"],
    ] {
        let output = cli(&root, &args);
        assert!(!output.status.success());
        assert!(!root.path().join(".aethyme/broker.db").exists());
    }
}

#[test]
fn cli_refuses_feature_pass_until_that_tree_is_the_fetched_default() {
    let (root, owner, failed) = fixture();
    let incident = json(cli(
        &root,
        &[
            "gates",
            "incidents",
            "confirm",
            &failed.to_string(),
            "unit::broken",
            "--session",
            &owner.to_string(),
            "--json",
        ],
    ));
    let id = incident[0]["id"].as_i64().unwrap();
    assert!(git(&root, &["ls-files", ".aethyme/broker.db"]).is_empty());
    std::fs::write(root.path().join("tracked.txt"), "feature repair\n").unwrap();
    git(&root, &["add", "tracked.txt"]);
    git(&root, &["commit", "-qm", "candidate"]);
    let candidate = git(&root, &["rev-parse", "HEAD"]);
    let tree = git(&root, &["rev-parse", "HEAD^{tree}"]);
    let mut broker = common::open_fixture_repository_broker(&root).unwrap();
    let (_, gates) = aethyme_broker::load_gates_at_commit(
        &aethyme_broker::GitRepo::discover(root.path()).unwrap(),
        &candidate,
    )
    .unwrap();
    let pass = broker
        .store()
        .record_gate_result_with_execution_profile(
            &NewGateResult {
                gate_name: "unit".into(),
                tree_hash: tree,
                definition_hash: gates[0].definition_hash.clone(),
                status: GateStatus::Pass,
                failure_class: None,
                exit_code: Some(0),
                duration_ms: Some(1),
                wait_duration_ms: None,
                first_output_ms: None,
                output_bytes: None,
                log_path: None,
                session_id: Some(owner),
            },
            &GateEnvironment::default(),
            Some(&GateCacheProvenance {
                execution_profile_digest: Some(format!("sha256:{}", "c".repeat(64))),
                profile_scope: "synthetic fixture".into(),
            }),
        )
        .unwrap();
    drop(broker);
    let args = [
        "gates",
        "incidents",
        "resolve",
        &id.to_string(),
        &pass.to_string(),
        "--json",
    ];
    assert!(!cli(&root, &args).status.success());
    assert_eq!(
        json(cli(&root, &["gates", "incidents", "list", "--json"])),
        incident
    );
    git(
        &root,
        &["update-ref", "refs/remotes/origin/main", &candidate],
    );
    let resolved = json(cli(&root, &args));
    assert_eq!(resolved[0]["resolved_run_id"], pass);
    assert_eq!(resolved[0]["resolved_base_commit"], candidate);
    assert!(resolved[0]["resolved_at"].is_i64());
    assert_eq!(json(cli(&root, &args)), resolved);
    let status = json(cli(&root, &["status", "--summary", "--json"]));
    assert!(
        !status["advice"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == "base.failure-incident")
    );
}
