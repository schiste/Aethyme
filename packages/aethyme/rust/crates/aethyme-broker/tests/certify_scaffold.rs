//! Certify/scaffold split tests: certify is ALWAYS read-only and
//! deterministic; scaffold generates drafts (byte-identical on re-run,
//! never overwriting); manifest detection.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use aethyme_broker::Gate;
use aethyme_broker::init::{self, CheckStatus};

fn sh(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hi\n").unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn write_agent_protocol(root: &Path) {
    std::fs::write(
        root.join("AGENTS.md"),
        "# Agent Instructions\n\n## Broker Coordination\n",
    )
    .unwrap();
}

/// Snapshot every file under `.aethyme` + `.gitignore` (path → bytes),
/// excluding the runtime db (its WAL bytes legitimately change).
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for rel in [".aethyme/gates.toml", ".aethyme/config.toml", ".gitignore"] {
        if let Ok(bytes) = std::fs::read(root.join(rel)) {
            out.insert(rel.to_string(), bytes);
        }
    }
    out
}

fn status_of(report: &init::InitReport, id: &str) -> CheckStatus {
    report
        .checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("missing check {id}"))
        .status
}

fn draft_created_and_load(root: &Path) -> Vec<Gate> {
    let report = init::draft_gates(root).unwrap();
    assert_eq!(status_of(&report, "gates.draft"), CheckStatus::Created);
    aethyme_broker::load_gates(root).unwrap()
}

fn assert_gate(gates: &[Gate], name: &str, command: &str, cost: i64, triggers: &[&str]) {
    let gate = gates
        .iter()
        .find(|gate| gate.name == name)
        .unwrap_or_else(|| panic!("missing gate {name}; got {gates:#?}"));
    assert_eq!(gate.command, command, "{name} command");
    assert_eq!(gate.cost, cost, "{name} cost");
    assert_eq!(
        gate.triggers,
        triggers
            .iter()
            .map(|trigger| trigger.to_string())
            .collect::<Vec<_>>(),
        "{name} triggers"
    );
}

fn assert_no_gate(gates: &[Gate], name: &str) {
    assert!(
        gates.iter().all(|gate| gate.name != name),
        "unexpected gate {name}; got {gates:#?}"
    );
}

#[test]
fn certify_is_always_read_only_and_scaffold_rerun_is_byte_identical() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());

    // Certify on a virgin repo: warns about what is missing, writes
    // NOTHING — including never creating the database.
    let before = snapshot(tmp.path());
    let report = init::certify(tmp.path()).unwrap();
    assert!(report.certified(), "missing config is warn, not fail");
    assert_eq!(
        status_of(&report, "certify.binary-version"),
        CheckStatus::Pass
    );
    assert_eq!(status_of(&report, "certify.gates"), CheckStatus::Warn);
    assert_eq!(status_of(&report, "certify.config"), CheckStatus::Warn);
    assert_eq!(status_of(&report, "certify.git-output"), CheckStatus::Pass);
    assert_eq!(snapshot(tmp.path()), before, "certify wrote nothing");
    assert!(
        !tmp.path().join(".aethyme/broker.db").exists(),
        "certify must never create the database"
    );

    // Scaffold: only the invariant broker artifacts — config skeleton,
    // gitignore block, database. Gate drafting is NOT scaffolding.
    let report = init::scaffold(tmp.path()).unwrap();
    assert!(report.certified());
    assert!(
        report.checks.iter().all(|c| !c.id.contains("gates")),
        "scaffold never touches gates"
    );
    for id in [
        "scaffold.config-toml",
        "scaffold.shared-activation",
        "scaffold.gitignore",
        "scaffold.broker-db",
    ] {
        assert_eq!(status_of(&report, id), CheckStatus::Created, "{id}");
    }
    assert!(
        !tmp.path().join(".aethyme/gates.toml").exists(),
        "no gates.toml from scaffold"
    );
    let first = snapshot(tmp.path());
    assert!(!first.is_empty());

    // Determinism: a second scaffold changes nothing, byte for byte.
    let report = init::scaffold(tmp.path()).unwrap();
    assert!(report.certified());
    for id in [
        "scaffold.config-toml",
        "scaffold.shared-activation",
        "scaffold.gitignore",
        "scaffold.broker-db",
    ] {
        assert_eq!(status_of(&report, id), CheckStatus::Pass, "{id}");
    }
    assert_eq!(snapshot(tmp.path()), first, "re-run is byte-identical");

    // Broker configuration makes the agent protocol mandatory.
    let report = init::certify(tmp.path()).unwrap();
    assert!(!report.certified());
    assert_eq!(
        status_of(&report, "certify.agents-protocol"),
        CheckStatus::Fail
    );
    assert_eq!(snapshot(tmp.path()), first, "certify still wrote nothing");

    write_agent_protocol(tmp.path());
    let report = init::certify(tmp.path()).unwrap();
    assert!(report.certified());
    assert_eq!(status_of(&report, "certify.config"), CheckStatus::Pass);
    assert_eq!(
        status_of(&report, "certify.shared-activation"),
        CheckStatus::Pass
    );
    assert_eq!(status_of(&report, "certify.gitignore"), CheckStatus::Pass);
    assert_eq!(snapshot(tmp.path()), first, "certify still wrote nothing");
}

#[test]
fn shared_activation_reaches_pre_enrollment_worktrees_and_certifies_upstream_visibility() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    init::scaffold(tmp.path()).unwrap();

    let marker = aethyme_broker::GitRepo::discover(tmp.path())
        .unwrap()
        .git_common_dir()
        .unwrap()
        .join(init::ACTIVATION_MARKER_RELPATH);
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        init::ACTIVATION_MARKER_CONTENT
    );

    let sibling = tmp.path().join("sibling-before-enrollment");
    sh(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "pre-enrollment",
            sibling.to_str().unwrap(),
            "HEAD",
        ],
    );
    let sibling_report = init::certify(&sibling).unwrap();
    assert_eq!(
        status_of(&sibling_report, "certify.shared-activation"),
        CheckStatus::Pass
    );
    assert_eq!(
        status_of(&sibling_report, "certify.checkout-enrollment"),
        CheckStatus::Warn
    );
    assert!(
        sibling_report
            .checks
            .iter()
            .find(|check| check.id == "certify.checkout-enrollment")
            .unwrap()
            .detail
            .contains("does not contain the enrollment commit")
    );

    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(
        tmp.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let unpublished = init::certify(tmp.path()).unwrap();
    assert_eq!(
        status_of(&unpublished, "certify.upstream-enrollment"),
        CheckStatus::Warn
    );

    sh(tmp.path(), &["add", ".aethyme/config.toml", ".gitignore"]);
    sh(tmp.path(), &["commit", "-qm", "enroll repository"]);
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    let published = init::certify(tmp.path()).unwrap();
    assert_eq!(
        status_of(&published, "certify.upstream-enrollment"),
        CheckStatus::Pass
    );
}

#[test]
fn certify_names_a_path_git_shim_that_decorates_known_empty_output() {
    const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    init_repo(&repo);

    let real_git = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("git"))
                .find(|candidate| candidate.is_file())
        })
        .expect("git on PATH");
    let shim = bin.join("git");
    std::fs::write(
        &shim,
        "#!/bin/sh\nif [ \"$1\" = status ]; then\n  \"$REAL_GIT\" \"$@\"\n  result=$?\n  [ \"$result\" -eq 0 ] && printf 'ok ✓'\n  exit \"$result\"\nfi\nexec \"$REAL_GIT\" \"$@\"\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&shim, permissions).unwrap();

    let original_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.clone()];
    paths.extend(std::env::split_paths(&original_path));
    let output = Command::new(CLI)
        .arg("certify")
        .current_dir(&repo)
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("REAL_GIT", real_git)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!output.status.success(), "{stdout}");
    assert!(stdout.contains("certify.git-output"), "{stdout}");
    assert!(stdout.contains("emitted 6 bytes"), "{stdout}");
    assert!(stdout.contains(&shim.display().to_string()), "{stdout}");
}

#[test]
fn config_schema_key_is_accepted_and_unknown_keys_warn_never_fail() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_agent_protocol(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    let config = tmp.path().join(".aethyme/config.toml");
    let certify_config = || {
        let report = init::certify(tmp.path()).unwrap();
        let check = report
            .checks
            .iter()
            .find(|c| c.id == "certify.config")
            .unwrap()
            .clone();
        assert!(report.certified(), "config issues must never fail certify");
        check
    };

    // The declared schema (and the full known surface) certifies clean.
    std::fs::write(
        &config,
        "schema = 1\n[promote]\nmode = \"manual\"\nbranch = \"b\"\n[leases]\nignore = [\"x/\"]\n",
    )
    .unwrap();
    let check = certify_config();
    assert_eq!(check.status, CheckStatus::Pass);
    assert!(check.detail.contains("schema 1"), "{}", check.detail);

    // Unknown section, unknown key in a known section, and a newer
    // schema number: WARN with the offenders named — never a failure.
    std::fs::write(
        &config,
        "schema = 2\n[promote]\nmodee = \"auto\"\n[future]\nx = 1\n",
    )
    .unwrap();
    let check = certify_config();
    assert_eq!(check.status, CheckStatus::Warn);
    for expected in ["schema = 2", "promote.modee", "future"] {
        assert!(check.detail.contains(expected), "{}", check.detail);
    }

    // The runtime consumers tolerate the same file (defaults + declared
    // values still load).
    let promote = aethyme_broker::PromoteConfig::load(tmp.path());
    assert_eq!(promote.branch, "aethyme/integration");
    assert!(promote.auto);

    // Malformed TOML is still a hard failure (broken, not future, intent).
    std::fs::write(&config, "[promote\n").unwrap();
    let report = init::certify(tmp.path()).unwrap();
    assert!(!report.certified());
}

#[test]
fn local_scaffold_uses_runtime_state_without_touching_gitignore() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());

    let report = init::scaffold_local(tmp.path()).unwrap();
    assert!(report.certified());
    assert!(tmp.path().join(".aethyme/config.toml").is_file());
    assert!(tmp.path().join(".aethyme/broker.db").is_file());
    assert!(!tmp.path().join(".gitignore").exists());
    assert_eq!(
        status_of(&report, "scaffold-local.config-toml"),
        CheckStatus::Created
    );

    let before_activation = init::certify(tmp.path()).unwrap();
    assert_eq!(
        status_of(&before_activation, "certify.agents-protocol"),
        CheckStatus::Fail
    );
    std::fs::create_dir_all(tmp.path().join(".aethyme/local")).unwrap();
    std::fs::write(tmp.path().join(".aethyme/local/enabled"), "schema = 1\n").unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/local/AGENTS.md"),
        "## Broker Coordination\n",
    )
    .unwrap();
    let activated = init::certify(tmp.path()).unwrap();
    assert_eq!(
        status_of(&activated, "certify.agents-protocol"),
        CheckStatus::Pass
    );
}

#[test]
fn linked_worktree_setup_targets_that_checkout_and_keeps_runtime_state_shared() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    let linked = tmp.path().join("linked");
    std::fs::create_dir(&main).unwrap();
    init_repo(&main);
    sh(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/setup",
            linked.to_str().unwrap(),
            "main",
        ],
    );
    std::fs::write(linked.join("Cargo.toml"), "[workspace]\n").unwrap();
    write_agent_protocol(&linked);

    let scaffold = init::scaffold(&linked).unwrap();
    assert!(scaffold.certified());
    let gates = init::draft_gates(&linked).unwrap();
    assert_eq!(status_of(&gates, "gates.draft"), CheckStatus::Created);

    assert!(linked.join(".aethyme/config.toml").is_file());
    assert!(linked.join(".aethyme/gates.toml").is_file());
    assert!(linked.join(".gitignore").is_file());
    assert!(!main.join(".aethyme/config.toml").exists());
    assert!(!main.join(".aethyme/gates.toml").exists());
    assert!(!main.join(".gitignore").exists());

    assert!(
        main.join(".aethyme/broker.db").is_file(),
        "broker runtime state remains shared through the primary checkout"
    );
    assert!(!linked.join(".aethyme/broker.db").exists());

    let report = init::certify(&linked).unwrap();
    assert!(report.certified(), "linked checkout should certify cleanly");
    assert_eq!(status_of(&report, "certify.config"), CheckStatus::Pass);
    assert_eq!(status_of(&report, "certify.gates"), CheckStatus::Warn);
    assert_eq!(status_of(&report, "certify.gitignore"), CheckStatus::Pass);
}

#[test]
fn certify_warns_when_valid_gates_are_not_tracked() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = 'unit'\ncommand = 'true'\n",
    )
    .unwrap();

    let report = init::certify(tmp.path()).unwrap();
    let gate_check = report
        .checks
        .iter()
        .find(|check| check.id == "certify.gates")
        .unwrap();
    assert_eq!(gate_check.status, CheckStatus::Warn);
    assert!(gate_check.detail.contains("untracked"));
    assert!(gate_check.detail.contains("spawned worktrees"));

    sh(tmp.path(), &["add", ".aethyme/gates.toml"]);
    sh(tmp.path(), &["commit", "-qm", "track gates"]);
    assert_eq!(
        status_of(&init::certify(tmp.path()).unwrap(), "certify.gates"),
        CheckStatus::Pass
    );
}

#[test]
fn gates_draft_detects_manifests_deterministically() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{"name":"x","scripts":{"test":"vitest run","lint":"eslint ."}}"#,
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("pyproject.toml"),
        "[tool.ruff]\nline-length = 100\n[tool.pytest.ini_options]\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    init_repo(tmp.path());

    // Gate drafting is its own adaptive command, not scaffold.
    let report = init::draft_gates(tmp.path()).unwrap();
    assert_eq!(status_of(&report, "gates.draft"), CheckStatus::Created);
    // Re-run: never overwritten.
    let report = init::draft_gates(tmp.path()).unwrap();
    assert_eq!(status_of(&report, "gates.draft"), CheckStatus::Pass);
    let gates = std::fs::read_to_string(tmp.path().join(".aethyme/gates.toml")).unwrap();
    for expected in [
        "name = \"cargo-test\"",
        "name = \"js-lint\"",
        "name = \"js-test\"",
        "name = \"ruff\"",
        "name = \"pytest\"",
    ] {
        assert!(gates.contains(expected), "missing {expected} in:\n{gates}");
    }
    assert!(gates.contains("REVIEW EVERY GATE"));
    // The draft parses as a valid gate config.
    let loaded = aethyme_broker::load_gates(tmp.path()).unwrap();
    assert_eq!(loaded.len(), 5);
    assert_gate(
        &loaded,
        "js-lint",
        "npm run lint --silent",
        1,
        &["**/*.js", "**/*.jsx", "**/*.ts", "**/*.tsx", "package.json"],
    );
    assert_gate(
        &loaded,
        "js-test",
        "npm test --silent",
        2,
        &["**/*.js", "**/*.jsx", "**/*.ts", "**/*.tsx", "package.json"],
    );
    let pytest = loaded.iter().find(|gate| gate.name == "pytest").unwrap();
    assert!(pytest.command.starts_with("python3 -c "));
    assert!(pytest.command.contains("pytest.console_main()"));
    assert!(!pytest.command.contains("-m pytest"));
    // No timestamps / absolute paths (determinism across machines & time).
    assert!(!gates.contains("202"), "no dates in generated files");
    assert!(!gates.contains(&tmp.path().to_string_lossy().into_owned()));
}

#[test]
fn guided_init_sets_up_a_fresh_repo_and_second_run_is_a_no_op() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    init_repo(tmp.path());

    // First run: all three phases execute; config + db are scaffolded and
    // (a manifest exists) a gates draft is written.
    let report = init::guided_init(tmp.path()).unwrap();
    assert!(report.certified());
    assert!(report.changed, "first run creates the missing artifacts");
    let scaffold = report.scaffold.as_ref().expect("certification passed");
    for id in [
        "scaffold.config-toml",
        "scaffold.gitignore",
        "scaffold.broker-db",
    ] {
        assert_eq!(status_of(scaffold, id), CheckStatus::Created, "{id}");
    }
    let gates = report.gates.as_ref().expect("no gates.toml before init");
    assert_eq!(status_of(gates, "gates.draft"), CheckStatus::Created);
    assert!(tmp.path().join(".aethyme/config.toml").exists());
    assert!(tmp.path().join(".aethyme/broker.db").exists());
    assert!(tmp.path().join(".aethyme/gates.toml").exists());

    // A configured repository must install agent policy before it certifies.
    assert!(!init::certify(tmp.path()).unwrap().certified());
    write_agent_protocol(tmp.path());

    // Second run: nothing changes (byte for byte), and the report says so.
    let first = snapshot(tmp.path());
    let report = init::guided_init(tmp.path()).unwrap();
    assert!(report.certified());
    assert!(!report.changed, "second run must be a no-op");
    let scaffold = report.scaffold.as_ref().unwrap();
    for id in [
        "scaffold.config-toml",
        "scaffold.gitignore",
        "scaffold.broker-db",
    ] {
        assert_eq!(status_of(scaffold, id), CheckStatus::Pass, "{id}");
    }
    assert!(
        report.gates.is_none(),
        "drafting is skipped once gates.toml exists"
    );
    assert_eq!(snapshot(tmp.path()), first, "re-run is byte-identical");
}

#[test]
fn guided_init_without_manifests_drafts_nothing_and_stays_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());

    let report = init::guided_init(tmp.path()).unwrap();
    assert!(report.certified());
    assert!(report.changed, "config + db are still scaffolded");
    let gates = report.gates.as_ref().expect("gates phase ran");
    assert_eq!(status_of(gates, "gates.draft"), CheckStatus::Warn);
    assert!(!tmp.path().join(".aethyme/gates.toml").exists());

    assert!(!init::certify(tmp.path()).unwrap().certified());
    write_agent_protocol(tmp.path());

    // No gates.toml means the draft phase runs again — and still writes
    // nothing, so the run as a whole reports no changes.
    let first = snapshot(tmp.path());
    let report = init::guided_init(tmp.path()).unwrap();
    assert!(!report.changed);
    assert_eq!(
        status_of(report.gates.as_ref().unwrap(), "gates.draft"),
        CheckStatus::Warn
    );
    assert_eq!(snapshot(tmp.path()), first);
}

#[test]
fn guided_init_stops_before_writing_when_certification_fails() {
    // Not a git repository: certification fails, so init must not write.
    let tmp = tempfile::tempdir().unwrap();
    let report = init::guided_init(tmp.path()).unwrap();
    assert!(!report.certified());
    assert!(report.scaffold.is_none(), "scaffold never ran");
    assert!(report.gates.is_none(), "gate drafting never ran");
    assert!(!report.changed);
    assert!(!tmp.path().join(".aethyme").exists(), "nothing was written");
}

#[test]
fn gates_draft_detects_node_package_scripts_and_lockfile_runners() {
    let pnpm = tempfile::tempdir().unwrap();
    std::fs::write(
        pnpm.path().join("package.json"),
        r#"{"scripts":{"test":"vitest run","lint":"eslint ."}}"#,
    )
    .unwrap();
    std::fs::write(
        pnpm.path().join("pnpm-lock.yaml"),
        "lockfileVersion: '9.0'\n",
    )
    .unwrap();
    init_repo(pnpm.path());

    let gates = draft_created_and_load(pnpm.path());
    assert_eq!(gates.len(), 2);
    assert_gate(
        &gates,
        "js-lint",
        "pnpm lint",
        1,
        &["**/*.js", "**/*.jsx", "**/*.ts", "**/*.tsx", "package.json"],
    );
    assert_gate(
        &gates,
        "js-test",
        "pnpm test",
        2,
        &["**/*.js", "**/*.jsx", "**/*.ts", "**/*.tsx", "package.json"],
    );

    let yarn = tempfile::tempdir().unwrap();
    std::fs::write(
        yarn.path().join("package.json"),
        r#"{"scripts":{"test":"vitest run"}}"#,
    )
    .unwrap();
    std::fs::write(yarn.path().join("yarn.lock"), "# yarn lockfile\n").unwrap();
    init_repo(yarn.path());

    let gates = draft_created_and_load(yarn.path());
    assert_eq!(gates.len(), 1);
    assert_gate(
        &gates,
        "js-test",
        "yarn test",
        2,
        &["**/*.js", "**/*.jsx", "**/*.ts", "**/*.tsx", "package.json"],
    );
}

#[test]
fn gates_draft_detects_go_module() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("go.mod"),
        "module example.com/x\n\ngo 1.22\n",
    )
    .unwrap();
    init_repo(tmp.path());

    let gates = draft_created_and_load(tmp.path());
    assert_eq!(gates.len(), 1);
    assert_gate(
        &gates,
        "go-test",
        "go test ./...",
        2,
        &["**/*.go", "go.mod", "go.sum"],
    );
}

#[test]
fn gates_draft_uses_makefile_test_target_as_fallback_only() {
    let make_only = tempfile::tempdir().unwrap();
    std::fs::write(
        make_only.path().join("Makefile"),
        ".PHONY: test\ntest:\n\ttrue\n",
    )
    .unwrap();
    init_repo(make_only.path());

    let gates = draft_created_and_load(make_only.path());
    assert_eq!(gates.len(), 1);
    assert_gate(&gates, "make-test", "make test", 2, &["**"]);

    let node_and_make = tempfile::tempdir().unwrap();
    std::fs::write(
        node_and_make.path().join("package.json"),
        r#"{"scripts":{"test":"vitest run"}}"#,
    )
    .unwrap();
    std::fs::write(
        node_and_make.path().join("Makefile"),
        ".PHONY: test\ntest:\n\ttrue\n",
    )
    .unwrap();
    init_repo(node_and_make.path());

    let gates = draft_created_and_load(node_and_make.path());
    assert_eq!(gates.len(), 1);
    assert_gate(
        &gates,
        "js-test",
        "npm test --silent",
        2,
        &["**/*.js", "**/*.jsx", "**/*.ts", "**/*.tsx", "package.json"],
    );
    assert_no_gate(&gates, "make-test");

    let lint_and_make = tempfile::tempdir().unwrap();
    std::fs::write(
        lint_and_make.path().join("package.json"),
        r#"{"scripts":{"lint":"eslint ."}}"#,
    )
    .unwrap();
    std::fs::write(
        lint_and_make.path().join("Makefile"),
        ".PHONY: test\ntest:\n\ttrue\n",
    )
    .unwrap();
    init_repo(lint_and_make.path());

    let gates = draft_created_and_load(lint_and_make.path());
    assert_eq!(gates.len(), 2);
    assert_gate(
        &gates,
        "js-lint",
        "npm run lint --silent",
        1,
        &["**/*.js", "**/*.jsx", "**/*.ts", "**/*.tsx", "package.json"],
    );
    assert_gate(&gates, "make-test", "make test", 2, &["**"]);
}

#[test]
fn gates_draft_without_manifests_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());

    let report = init::draft_gates(tmp.path()).unwrap();
    assert_eq!(status_of(&report, "gates.draft"), CheckStatus::Warn);
    assert!(!tmp.path().join(".aethyme/gates.toml").exists());
    assert!(matches!(
        aethyme_broker::load_gates(tmp.path()),
        Err(aethyme_broker::GateConfigError::Missing(_))
    ));
}

#[test]
fn existing_files_are_never_touched_and_gitignore_appends_preserving_content() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"mine\"\ncommand = \"true\"\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "node_modules/\n").unwrap();

    let report = init::scaffold(tmp.path()).unwrap();
    assert!(report.certified());
    let draft = init::draft_gates(tmp.path()).unwrap();
    assert_eq!(status_of(&draft, "gates.draft"), CheckStatus::Pass);
    let gates = std::fs::read_to_string(tmp.path().join(".aethyme/gates.toml")).unwrap();
    assert!(gates.contains("mine"), "user's gates.toml untouched");
    assert!(!gates.contains("Draft generated"));
    let gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert!(
        gitignore.starts_with("node_modules/\n"),
        "content preserved"
    );
    assert!(gitignore.contains("aethyme-broker:begin"));
    assert!(gitignore.contains(".aethyme/reports/"));
    assert!(gitignore.contains(".aethyme/worktrees/"));
    assert!(gitignore.contains(".aethyme/broker-advisory.md"));
    assert!(gitignore.contains(".aethyme/generated/experience-telemetry.jsonl"));
    assert!(gitignore.contains(".aethyme/generated/experience-status.json"));
    assert!(gitignore.contains(".aethyme/generated/experience-status.md"));
}

#[test]
fn scaffold_upgrades_an_older_managed_gitignore_block_for_reports() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let old_block = "custom-before/\n\n\
# aethyme-broker:begin (managed block — do not edit inside)\n\
.aethyme/broker.db*\n\
.aethyme/logs/\n\
.aethyme/run/\n\
.aethyme/worktrees/\n\
.aethyme/broker-action-required.md\n\
# aethyme-broker:end\n\
custom-after/\n";
    std::fs::write(tmp.path().join(".gitignore"), old_block).unwrap();

    let before = init::certify(tmp.path()).unwrap();
    assert_eq!(status_of(&before, "certify.gitignore"), CheckStatus::Warn);
    let scaffold = init::scaffold(tmp.path()).unwrap();
    assert_eq!(
        status_of(&scaffold, "scaffold.gitignore"),
        CheckStatus::Created
    );
    let gitignore = std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert!(gitignore.contains("custom-before/"));
    assert!(gitignore.contains("custom-after/"));
    assert!(gitignore.contains(".aethyme/reports/"));
    assert!(gitignore.contains(".aethyme/generated/experience-telemetry.jsonl"));
    assert_eq!(gitignore.matches("aethyme-broker:begin").count(), 1);
    assert_eq!(
        status_of(&init::certify(tmp.path()).unwrap(), "certify.gitignore"),
        CheckStatus::Pass
    );
}
