use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{
    AdvisoryAudience, AdvisoryProducer, Broker, HostResourceCoordinator, MergeStatus,
    ReadinessDimensionId, ReadinessReport, ReadinessState, RepositoryOperatingMode,
    RepositoryReadinessMode,
};
use aethyme_engine::index_store::materialize_graph_store;
use aethyme_engine::map::RepositoryMap;
use aethyme_graph_indexer::{IndexerContext, WalkOptions, index_repo_to_disk, link_repo};
use aethyme_graph_storage::{bootstrap_repo, write_graph_authority_manifest};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Readiness Test")
        .env("GIT_AUTHOR_EMAIL", "readiness@example.com")
        .env("GIT_COMMITTER_NAME", "Readiness Test")
        .env("GIT_COMMITTER_EMAIL", "readiness@example.com")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join("README.md"), "readiness fixture\n").unwrap();
    git(repo.path(), &["add", "README.md"]);
    git(repo.path(), &["commit", "-qm", "test: initialize fixture"]);
    repo
}

fn host_state_path(root: &Path) -> PathBuf {
    root.join("host-state")
}

fn run_readiness(root: &Path, host_state: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .arg("readiness")
        .args(args)
        .current_dir(root)
        .env("AETHYME_HOST_STATE_DIR", host_state)
        .output()
        .expect("run readiness")
}

fn readiness_json(root: &Path, host_state: &Path) -> (Output, ReadinessReport) {
    let output = run_readiness(root, host_state, &["--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = serde_json::from_slice(&output.stdout).expect("stable readiness JSON");
    (output, report)
}

fn run_broker(root: &Path, host_state: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(root)
        .env("AETHYME_HOST_STATE_DIR", host_state)
        .output()
        .expect("run broker CLI")
}

fn dimension(
    report: &ReadinessReport,
    id: ReadinessDimensionId,
) -> &aethyme_broker::ReadinessDimension {
    report
        .dimensions
        .iter()
        .find(|dimension| dimension.id == id)
        .expect("readiness dimension")
}

fn write_canonical_contract(root: &Path, schema: u32) {
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".aethyme/repository.json"),
        format!("{{\"schema_version\":{schema}}}\n"),
    )
    .unwrap();
    std::fs::write(root.join(".aethyme/config.toml"), "schema = 1\n").unwrap();
}

fn write_valid_gates(root: &Path) {
    std::fs::write(
        root.join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"fast\"\ncommand = \"true\"\ncost = 0\n",
    )
    .unwrap();
}

fn write_valid_preparation(root: &Path) {
    std::fs::write(
        root.join(".aethyme/prepare.toml"),
        "schema_version = 1\n\n[[steps]]\nname = \"dependencies\"\ncommand = [\"true\"]\noutputs = [\".prepared\"]\nrequired_for_hooks = true\n",
    )
    .unwrap();
}

fn initialize_host_state(state: &Path) {
    let path = state.join("host-resources.db");
    drop(HostResourceCoordinator::open(&path).expect("initialize host resource state"));
}

fn canonical_ready_fixture(root: &Path, state: &Path) {
    write_canonical_contract(root, aethyme_broker::REPOSITORY_SCHEMA_VERSION);
    write_valid_gates(root);
    write_valid_preparation(root);
    aethyme_broker::init::scaffold(root).unwrap();
    aethyme_enhance::deploy::deploy(root, true).unwrap();
    initialize_host_state(state);
}

#[test]
fn virgin_repository_is_undeployed_and_inspection_creates_nothing() {
    let repo = init_repo();
    let state = host_state_path(repo.path());

    let (_, report) = readiness_json(repo.path(), &state);

    assert_eq!(report.schema_version, 2);
    assert_eq!(report.repository_mode, RepositoryReadinessMode::Absent);
    assert_eq!(report.operating_mode, RepositoryOperatingMode::Undeployed);
    assert!(!repo.path().join(".aethyme").exists());
    assert!(!state.exists());
}

#[test]
fn conflict_only_and_full_parallel_readiness_are_distinct() {
    let repo = init_repo();
    let state = host_state_path(repo.path());
    write_canonical_contract(repo.path(), aethyme_broker::REPOSITORY_SCHEMA_VERSION);

    let (_, conflict_only) = readiness_json(repo.path(), &state);
    assert_eq!(
        conflict_only.operating_mode,
        RepositoryOperatingMode::ConflictOnly
    );

    write_valid_gates(repo.path());
    write_valid_preparation(repo.path());
    aethyme_broker::init::scaffold(repo.path()).unwrap();
    aethyme_enhance::deploy::deploy(repo.path(), true).unwrap();
    initialize_host_state(&state);
    let broker_before = std::fs::read(repo.path().join(".aethyme/broker.db")).unwrap();
    let host_before = std::fs::read(state.join("host-resources.db")).unwrap();
    let (_, ready) = readiness_json(repo.path(), &state);
    assert_eq!(ready.operating_mode, RepositoryOperatingMode::ParallelReady);

    assert!(
        run_readiness(repo.path(), &state, &["--require", "parallel-ready"])
            .status
            .success()
    );
    assert_eq!(
        std::fs::read(repo.path().join(".aethyme/broker.db")).unwrap(),
        broker_before,
        "readiness must not migrate, refresh sessions, or refresh leases"
    );
    assert_eq!(
        std::fs::read(state.join("host-resources.db")).unwrap(),
        host_before,
        "readiness must not mutate host coordination state"
    );
    assert!(
        !repo
            .path()
            .join(".aethyme/logs/command-metrics.jsonl")
            .exists(),
        "readiness must not create command telemetry"
    );
}

#[test]
fn readiness_includes_read_only_maintainer_history_recommendations() {
    let repo = init_repo();
    let state = host_state_path(repo.path());
    canonical_ready_fixture(repo.path(), &state);

    let mut broker = Broker::open(repo.path()).unwrap();
    let session = broker
        .adopt(repo.path(), Some("must never enter readiness output"))
        .unwrap();
    for index in 1..=3 {
        let head = format!("{index:040x}");
        let entry = broker.store().submit(session.id, &head, &head).unwrap();
        broker
            .store()
            .set_merge_status(
                entry.id,
                MergeStatus::Conflict,
                None,
                Some(r#"{"conflicts":["src/shared.rs"]}"#),
            )
            .unwrap();
    }
    drop(broker);

    let database_path = repo.path().join(".aethyme/broker.db");
    let before = std::fs::read(&database_path).unwrap();
    let (_, report) = readiness_json(repo.path(), &state);

    assert_eq!(report.maintainer_history_state, "ready");
    assert_eq!(report.maintainer_advisories.len(), 1);
    let recommendation = &report.maintainer_advisories[0];
    assert_eq!(recommendation.audience, AdvisoryAudience::Maintainer);
    assert_eq!(recommendation.producer, AdvisoryProducer::ConflictHistory);
    assert_eq!(recommendation.paths, ["src/shared.rs"]);
    assert_eq!(std::fs::read(database_path).unwrap(), before);
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("must never enter readiness output"));
}

#[test]
fn guided_init_reports_one_post_run_readiness_snapshot() {
    let text_repo = init_repo();
    let text_state = host_state_path(text_repo.path());
    let text = run_broker(text_repo.path(), &text_state, &["init"]);
    assert!(
        text.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&text.stdout),
        String::from_utf8_lossy(&text.stderr)
    );
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(stdout.contains("Repository initialized."));
    assert_eq!(stdout.matches("Operating mode:").count(), 1);
    assert!(stdout.contains("Operating mode: conflict_only"));
    assert!(stdout.contains("Coordination: ready"));
    assert!(stdout.contains("Agent context: not ready"));
    assert!(stdout.contains("Validation: limited"));
    assert!(stdout.contains("Parallel execution: not ready"));
    assert!(stdout.contains("Next actions:"));

    let json_repo = init_repo();
    let json_state = host_state_path(json_repo.path());
    let json = run_broker(json_repo.path(), &json_state, &["init", "--json"]);
    assert!(json.status.success());
    let document: serde_json::Value = serde_json::from_slice(&json.stdout)
        .expect("--json emits exactly one JSON document with no prose");
    assert_eq!(document["readiness"]["operating_mode"], "conflict_only");
    let json_text = String::from_utf8(json.stdout).unwrap();
    let positions = ["certify", "scaffold", "gates", "changed", "readiness"]
        .map(|field| json_text.find(&format!("\n  \"{field}\":")).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));

    let certify_repo = init_repo();
    let certify = run_broker(
        certify_repo.path(),
        &host_state_path(certify_repo.path()),
        &["certify", "--json"],
    );
    let certify_document: serde_json::Value = serde_json::from_slice(&certify.stdout).unwrap();
    assert!(certify_document.get("readiness").is_none());
}

#[test]
fn local_only_deployment_is_reported_without_canonicalizing_it() {
    let repo = init_repo();
    let state = host_state_path(repo.path());
    aethyme_enhance::local::install_bridge(repo.path()).unwrap();
    aethyme_enhance::local::deploy(repo.path(), true).unwrap();
    std::fs::write(
        repo.path().join(".aethyme/local/repository.json"),
        format!(
            "{{\"schema_version\":{}}}\n",
            aethyme_broker::REPOSITORY_SCHEMA_VERSION
        ),
    )
    .unwrap();
    std::fs::write(repo.path().join(".aethyme/config.toml"), "schema = 1\n").unwrap();
    write_valid_gates(repo.path());
    write_valid_preparation(repo.path());
    aethyme_broker::init::scaffold_local(repo.path()).unwrap();
    initialize_host_state(&state);

    let (_, report) = readiness_json(repo.path(), &state);

    assert_eq!(report.repository_mode, RepositoryReadinessMode::LocalOnly);
    assert_eq!(
        dimension(&report, ReadinessDimensionId::RepositoryDeployment).state,
        ReadinessState::Ready
    );
}

#[test]
fn missing_stale_and_invalid_agent_inputs_are_classified() {
    let repo = init_repo();
    let state = host_state_path(repo.path());
    canonical_ready_fixture(repo.path(), &state);
    let onboarding = repo
        .path()
        .join(aethyme_enhance::onboarding::ONBOARDING_JSON_PATH);

    std::fs::write(&onboarding, "stale\n").unwrap();
    let (_, stale) = readiness_json(repo.path(), &state);
    assert_eq!(
        dimension(&stale, ReadinessDimensionId::AgentContext).state,
        ReadinessState::Limited
    );

    std::fs::remove_file(&onboarding).unwrap();
    let (_, missing) = readiness_json(repo.path(), &state);
    assert_eq!(
        dimension(&missing, ReadinessDimensionId::AgentContext).state,
        ReadinessState::NotReady
    );

    std::fs::write(repo.path().join(".aethyme/gates.toml"), "not toml = [").unwrap();
    let (_, invalid) = readiness_json(repo.path(), &state);
    assert_eq!(
        dimension(&invalid, ReadinessDimensionId::Validation).state,
        ReadinessState::NotReady
    );
}

#[test]
fn missing_and_inaccessible_host_state_remain_observational() {
    let repo = init_repo();
    let missing_state = host_state_path(repo.path());
    canonical_ready_fixture(repo.path(), &missing_state);
    std::fs::remove_file(missing_state.join("host-resources.db")).unwrap();

    let (_, missing) = readiness_json(repo.path(), &missing_state);
    assert_eq!(
        dimension(&missing, ReadinessDimensionId::ParallelExecution).state,
        ReadinessState::Ready
    );
    assert!(!missing_state.join("host-resources.db").exists());

    let not_a_directory = repo.path().join("not-a-host-directory");
    std::fs::write(&not_a_directory, "file\n").unwrap();
    let (_, inaccessible) = readiness_json(repo.path(), &not_a_directory);
    assert_eq!(
        dimension(&inaccessible, ReadinessDimensionId::ParallelExecution).state,
        ReadinessState::Unknown
    );
}

#[test]
fn missing_or_invalid_preparation_prevents_false_parallel_readiness() {
    let repo = init_repo();
    let state = host_state_path(repo.path());
    write_canonical_contract(repo.path(), aethyme_broker::REPOSITORY_SCHEMA_VERSION);
    write_valid_gates(repo.path());
    aethyme_broker::init::scaffold(repo.path()).unwrap();
    aethyme_enhance::deploy::deploy(repo.path(), true).unwrap();
    initialize_host_state(&state);

    let (_, missing) = readiness_json(repo.path(), &state);
    let parallel = dimension(&missing, ReadinessDimensionId::ParallelExecution);
    assert_eq!(parallel.state, ReadinessState::NotReady);
    assert!(parallel.summary.contains("not proven"));
    assert!(
        parallel
            .evidence
            .iter()
            .any(|evidence| evidence.summary.contains(".aethyme/prepare.toml"))
    );

    std::fs::write(
        repo.path().join(".aethyme/prepare.toml"),
        "schema_version = 99\nsteps = []\n",
    )
    .unwrap();
    let (_, invalid) = readiness_json(repo.path(), &state);
    assert_eq!(
        dimension(&invalid, ReadinessDimensionId::ParallelExecution).state,
        ReadinessState::NotReady
    );
}

#[test]
fn graph_disabled_ready_and_stale_states_use_exact_content_evidence() {
    let repo = init_repo();
    let state = host_state_path(repo.path());
    let (_, disabled) = readiness_json(repo.path(), &state);
    assert_eq!(
        dimension(&disabled, ReadinessDimensionId::GraphAvailability).state,
        ReadinessState::NotApplicable
    );

    std::fs::create_dir_all(repo.path().join(".aethyme")).unwrap();
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/lib.rs"), "pub fn ready() {}\n").unwrap();
    std::fs::write(
        repo.path().join(".aethyme/config.toml"),
        "[graph]\nauthority = \"committed_fragments\"\nrepository = \"example/repo\"\n",
    )
    .unwrap();
    let version = env!("CARGO_PKG_VERSION");
    bootstrap_repo(repo.path(), version).unwrap();
    git(
        repo.path(),
        &[
            "add",
            ".aethyme/config.toml",
            ".aethyme/engine-version",
            "src/lib.rs",
        ],
    );
    git(repo.path(), &["commit", "-qm", "test: enable graph"]);

    let context = IndexerContext::new("example/repo", repo.path().to_path_buf(), version).unwrap();
    index_repo_to_disk(&context, &WalkOptions::default()).unwrap();
    link_repo(&context).unwrap();
    write_graph_authority_manifest(repo.path(), "HEAD", "example/repo", version).unwrap();
    git(
        repo.path(),
        &["add", ".aethyme/engine-version", ".aethyme/graph"],
    );
    git(repo.path(), &["commit", "-qm", "test: commit graph"]);
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let (map, _) = RepositoryMap::build_from_fragments(repo.path()).unwrap();
    materialize_graph_store(repo.path(), &map, head.trim()).unwrap();

    let (_, ready) = readiness_json(repo.path(), &state);
    assert_eq!(
        dimension(&ready, ReadinessDimensionId::GraphAvailability).state,
        ReadinessState::Ready
    );

    std::fs::write(repo.path().join("src/lib.rs"), "pub fn changed() {}\n").unwrap();
    git(repo.path(), &["add", "src/lib.rs"]);
    git(repo.path(), &["commit", "-qm", "test: stale graph"]);
    let (_, stale) = readiness_json(repo.path(), &state);
    assert_eq!(
        dimension(&stale, ReadinessDimensionId::GraphAvailability).state,
        ReadinessState::Limited
    );
}

#[test]
fn older_and_newer_repository_schemas_fail_only_explicit_requirements() {
    for schema in [0, aethyme_broker::REPOSITORY_SCHEMA_VERSION + 1] {
        let repo = init_repo();
        let state = host_state_path(repo.path());
        write_canonical_contract(repo.path(), schema);

        let (default, report) = readiness_json(repo.path(), &state);
        assert!(default.status.success());
        assert_eq!(report.operating_mode, RepositoryOperatingMode::Invalid);
        assert_eq!(
            dimension(&report, ReadinessDimensionId::UpgradeCompatibility).state,
            ReadinessState::NotReady
        );

        let required = run_readiness(repo.path(), &state, &["--require", "conflict-only"]);
        assert_eq!(required.status.code(), Some(1));
    }
}

#[test]
fn readiness_json_field_and_dimension_order_is_stable() {
    let repo = init_repo();
    let state = host_state_path(repo.path());
    let (output, report) = readiness_json(repo.path(), &state);
    let text = String::from_utf8(output.stdout).unwrap();
    let fields = [
        "\"schema_version\"",
        "\"repository_mode\"",
        "\"operating_mode\"",
        "\"source_head\"",
        "\"deployment_digest\"",
        "\"dimensions\"",
        "\"blockers\"",
        "\"warnings\"",
    ];
    let positions = fields
        .iter()
        .map(|field| text.find(field).expect("JSON field"))
        .collect::<Vec<_>>();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        report
            .dimensions
            .iter()
            .map(|dimension| dimension.id)
            .collect::<Vec<_>>(),
        ReadinessDimensionId::ORDERED
    );
}
