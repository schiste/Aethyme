use std::path::Path;
use std::process::{Command, Output};

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

fn run(repo: &Path, state: &Path, args: &[&str]) -> Output {
    common::broker_cli(CLI, args)
        .current_dir(repo)
        .env("AETHYME_HOST_STATE_DIR", state)
        .output()
        .unwrap()
}

fn success_json(output: Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    git(temp.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(temp.path().join(".aethyme")).unwrap();
    std::fs::write(
        temp.path().join(".gitignore"),
        "/.aethyme/broker.db*\n/.aethyme/run/\n/node_modules/\n/target/\n/.venv/\n",
    )
    .unwrap();
    for lockfile in [
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "Cargo.lock",
        "uv.lock",
    ] {
        std::fs::write(temp.path().join(lockfile), format!("{lockfile}\n")).unwrap();
    }
    std::fs::write(temp.path().join("work.txt"), "initial\n").unwrap();
    std::fs::write(
        temp.path().join(".aethyme/prepare.toml"),
        r#"schema_version = 1

[[runtimes]]
name = "shell"
command = ["sh", "-c", "touch runtime-probe-must-not-run"]

[[steps]]
name = "npm-pnpm-yarn"
command = ["sh", "-c", "test -n \"$AETHYME_PREPARE_CACHE_DIR\" && mkdir -p node_modules"]
offline_command = ["sh", "-c", "mkdir -p node_modules"]
inputs = ["package-lock.json", "pnpm-lock.yaml", "yarn.lock"]
outputs = ["node_modules/"]
cache = "repository_shared"
required_for_hooks = true

[[steps]]
name = "cargo"
command = ["sh", "-c", "mkdir -p target"]
offline_command = ["sh", "-c", "mkdir -p target"]
inputs = ["Cargo.lock"]
outputs = ["target/"]

[[steps]]
name = "python"
command = ["sh", "-c", "mkdir -p .venv"]
offline_command = ["sh", "-c", "mkdir -p .venv"]
inputs = ["uv.lock"]
outputs = [".venv/"]
"#,
    )
    .unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-qm", "fixture"]);
    temp
}

#[test]
fn low_space_preparation_preserves_state_and_does_not_run_install() {
    let temp = fixture();
    let state = temp.path().join("host-state");
    let adopted = success_json(run(
        temp.path(),
        &state,
        &["start", "--adopt", ".", "--task", "prepare", "--json"],
    ));
    let session = adopted["id"].as_i64().unwrap().to_string();
    let output = common::broker_cli(CLI, &["submit", "prepare", "--session", &session, "--json"])
        .current_dir(temp.path())
        .env("AETHYME_HOST_STATE_DIR", &state)
        .env("AETHYME_TEST_AVAILABLE_BYTES", "0")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        json_error::message(&output).contains("dependency preparation storage"),
        "{}",
        json_error::message(&output)
    );
    for path in [
        "node_modules",
        "target",
        ".venv",
        "runtime-probe-must-not-run",
    ] {
        assert!(
            !temp.path().join(path).exists(),
            "preparation created {path} under pressure"
        );
    }
    let status = success_json(run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "status",
            "--session",
            &session,
            "--json",
        ],
    ));
    assert_eq!(status["state"], "required");
}

#[test]
fn preparation_rechecks_headroom_before_the_next_step() {
    let temp = fixture();
    let config_path = temp.path().join(".aethyme/prepare.toml");
    let mut config: toml::Value =
        toml::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
    config["steps"][0]["command"] = toml::Value::Array(
        [
            "sh",
            "-c",
            "mkdir -p node_modules && printf 0 > \"${AETHYME_TEST_AVAILABLE_BYTES#@}\"",
        ]
        .into_iter()
        .map(|value| toml::Value::String(value.into()))
        .collect(),
    );
    std::fs::write(&config_path, toml::to_string(&config).unwrap()).unwrap();
    git(temp.path(), &["add", ".aethyme/prepare.toml"]);
    git(temp.path(), &["commit", "-qm", "fixture storage pressure"]);
    let state = temp.path().join("host-state");
    let adopted = success_json(run(
        temp.path(),
        &state,
        &["start", "--adopt", ".", "--task", "prepare", "--json"],
    ));
    let session = adopted["id"].as_i64().unwrap().to_string();
    let reading = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(reading.path(), u64::MAX.to_string()).unwrap();
    let output = common::broker_cli(CLI, &["submit", "prepare", "--session", &session, "--json"])
        .current_dir(temp.path())
        .env("AETHYME_HOST_STATE_DIR", &state)
        .env(
            "AETHYME_TEST_AVAILABLE_BYTES",
            format!("@{}", reading.path().display()),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        json_error::message(&output).contains("dependency preparation storage"),
        "{}",
        json_error::message(&output)
    );
    assert!(temp.path().join("node_modules").is_dir());
    assert!(
        !temp.path().join("target").exists(),
        "second install ran after reserve was exhausted"
    );
    let status = success_json(run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "status",
            "--session",
            &session,
            "--json",
        ],
    ));
    assert_eq!(status["state"], "failed");
}

#[test]
fn current_preparation_can_be_read_when_disk_is_full() {
    let temp = fixture();
    let state = temp.path().join("host-state");
    let adopted = success_json(run(
        temp.path(),
        &state,
        &["start", "--adopt", ".", "--task", "prepare", "--json"],
    ));
    let session = adopted["id"].as_i64().unwrap().to_string();
    success_json(run(
        temp.path(),
        &state,
        &["submit", "prepare", "--session", &session, "--json"],
    ));
    let output = common::broker_cli(CLI, &["submit", "prepare", "--session", &session, "--json"])
        .current_dir(temp.path())
        .env("AETHYME_HOST_STATE_DIR", &state)
        .env("AETHYME_TEST_AVAILABLE_BYTES", "0")
        .output()
        .unwrap();
    let report = success_json(output);
    assert_eq!(report["state"], "current");
    assert_eq!(report["steps"], serde_json::json!([]));
}

#[test]
fn preparation_is_explicit_language_neutral_and_invalidates_on_input_change() {
    let temp = fixture();
    let state = temp.path().join("host-state");
    let adopted = success_json(run(
        temp.path(),
        &state,
        &["start", "--adopt", ".", "--task", "prepare", "--json"],
    ));
    let session = adopted["id"].as_i64().unwrap().to_string();
    assert_eq!(adopted["preparation"]["state"], "required");
    assert!(!temp.path().join("runtime-probe-must-not-run").exists());

    for output in ["node_modules", "target", ".venv"] {
        assert!(!temp.path().join(output).exists(), "adopt ran preparation");
    }
    let required = success_json(run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "status",
            "--session",
            &session,
            "--json",
        ],
    ));
    assert_eq!(required["state"], "required");
    assert_eq!(
        required["next_action"],
        format!("aethyme broker submit prepare --session {session}")
    );

    std::fs::write(temp.path().join("work.txt"), "staged work\n").unwrap();
    git(temp.path(), &["add", "work.txt"]);
    let blocked = run(temp.path(), &state, &["hooks", "pre-commit"]);
    assert!(!blocked.status.success());
    let error = String::from_utf8_lossy(&blocked.stderr);
    assert!(
        error.contains("Your staged changes remain unchanged"),
        "{error}"
    );
    assert!(
        error.contains(&format!(
            "aethyme broker submit prepare --session {session}"
        )),
        "{error}"
    );

    let prepared = success_json(run(
        temp.path(),
        &state,
        &["submit", "prepare", "--session", &session, "--json"],
    ));
    assert_eq!(prepared["state"], "current");
    assert_eq!(prepared["shared_cache_coordinated"], true);
    assert_eq!(prepared["steps"].as_array().map(Vec::len), Some(3));
    assert!(!temp.path().join("runtime-probe-must-not-run").exists());
    for output in ["node_modules", "target", ".venv"] {
        assert!(temp.path().join(output).is_dir());
    }
    let accepted = run(temp.path(), &state, &["hooks", "pre-commit"]);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );

    let current = success_json(run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "status",
            "--session",
            &session,
            "--json",
        ],
    ));
    assert_eq!(current["state"], "current");
    std::fs::write(temp.path().join("Cargo.lock"), "changed bytes\n").unwrap();
    let stale = success_json(run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "status",
            "--session",
            &session,
            "--json",
        ],
    ));
    assert_eq!(stale["state"], "stale");
    assert_ne!(stale["expected_digest"], stale["recorded_digest"]);
}

#[test]
fn offline_preparation_uses_only_explicit_offline_commands() {
    let temp = fixture();
    let state = temp.path().join("host-state");
    let adopted = success_json(run(
        temp.path(),
        &state,
        &["start", "--adopt", ".", "--task", "offline", "--json"],
    ));
    let session = adopted["id"].as_i64().unwrap().to_string();
    let prepared = success_json(run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "--session",
            &session,
            "--offline",
            "--json",
        ],
    ));
    assert_eq!(prepared["offline"], true);
    assert_eq!(prepared["state"], "current");
}

#[test]
fn failed_and_interrupted_preparation_states_are_explicit() {
    let temp = fixture();
    let state = temp.path().join("host-state");
    let adopted = success_json(run(
        temp.path(),
        &state,
        &["start", "--adopt", ".", "--task", "failure", "--json"],
    ));
    let session_id = adopted["id"].as_i64().unwrap();
    let session = session_id.to_string();

    std::fs::write(
        temp.path().join(".aethyme/prepare.toml"),
        r#"schema_version = 1

[[steps]]
name = "fails"
command = ["sh", "-c", "exit 7"]
inputs = ["Cargo.lock"]
outputs = ["target/"]
required_for_hooks = true
"#,
    )
    .unwrap();
    let failed = run(
        temp.path(),
        &state,
        &["submit", "prepare", "--session", &session, "--json"],
    );
    assert!(!failed.status.success());
    let status = success_json(run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "status",
            "--session",
            &session,
            "--json",
        ],
    ));
    assert_eq!(status["state"], "failed");
    assert!(status["reason"].as_str().unwrap().contains("fails"));

    let state_path = temp.path().join(format!(
        ".aethyme/run/preparation/session-{session_id}.json"
    ));
    std::fs::write(
        &state_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "session_id": session_id,
            "state": "in_progress",
            "digest": status["expected_digest"],
            "source_digest": status["source_digest"],
            "started_at": 1,
            "completed_at": null,
            "failed_step": null,
            "exit_code": null,
            "host_lease_id": "lease-evidence",
            "host_lease_generation": 3
        }))
        .unwrap(),
    )
    .unwrap();
    let interrupted = success_json(run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "status",
            "--session",
            &session,
            "--json",
        ],
    ));
    assert_eq!(interrupted["state"], "in_progress");
    let reason = interrupted["reason"].as_str().unwrap();
    assert!(reason.contains("lease-evidence"), "{reason}");
    assert!(reason.contains("generation 3"), "{reason}");
}

/// A repository that declares gates but no preparation config (#131 finding 2).
fn gates_without_preparation_fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    git(temp.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(temp.path().join(".aethyme")).unwrap();
    std::fs::write(
        temp.path().join(".gitignore"),
        "/.aethyme/broker.db*\n/.aethyme/run/\n/node_modules/\n",
    )
    .unwrap();
    std::fs::write(
        temp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"quality\"\ncommand = \"true\"\ncost = 1\n",
    )
    .unwrap();
    std::fs::write(temp.path().join("work.txt"), "initial\n").unwrap();
    git(temp.path(), &["add", "-A"]);
    git(
        temp.path(),
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "init",
        ],
    );
    // A dependency directory the primary checkout has and a fresh worktree will not.
    std::fs::create_dir_all(temp.path().join("node_modules/pkg")).unwrap();
    temp
}

/// `not_configured` must not read as "nothing to do" when the repository
/// declares gate commands a fresh worktree cannot satisfy.
#[test]
fn unconfigured_preparation_names_the_consequence_when_gates_exist() {
    let repo = gates_without_preparation_fixture();
    let state = tempfile::tempdir().unwrap();
    let started = success_json(run(
        repo.path(),
        state.path(),
        &["start", "--task", "gates without preparation", "--json"],
    ));
    let session = started["id"].as_i64().unwrap().to_string();

    let status = success_json(run(
        repo.path(),
        state.path(),
        &[
            "submit",
            "prepare",
            "status",
            "--session",
            &session,
            "--json",
        ],
    ));
    assert_eq!(status["state"], "not_configured");
    let reason = status["reason"].as_str().unwrap();
    assert!(
        reason.contains("gate command"),
        "the reason must state that gates exist: {reason}"
    );
    assert!(
        reason.contains("node_modules"),
        "the reason must name the ignored path the worktree lacks: {reason}"
    );
    assert!(
        status["next_action"].is_string(),
        "a null next_action reads as 'nothing to do here'"
    );
    assert_eq!(
        status["missing_outputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["node_modules"]
    );
}

/// With no gates declared there is nothing to warn about, so the report stays
/// quiet rather than inventing an obligation.
#[test]
fn unconfigured_preparation_stays_quiet_without_gates() {
    let repo = gates_without_preparation_fixture();
    std::fs::remove_file(repo.path().join(".aethyme/gates.toml")).unwrap();
    let state = tempfile::tempdir().unwrap();
    let started = success_json(run(
        repo.path(),
        state.path(),
        &["start", "--task", "no gates", "--json"],
    ));
    let session = started["id"].as_i64().unwrap().to_string();

    let status = success_json(run(
        repo.path(),
        state.path(),
        &[
            "submit",
            "prepare",
            "status",
            "--session",
            &session,
            "--json",
        ],
    ));
    assert_eq!(status["state"], "not_configured");
    assert_eq!(
        status["reason"],
        "repository declares no dependency preparation"
    );
    assert!(status["next_action"].is_null());
}

/// A rotation clears the whole repository store, and the step that triggered it
/// still has to run against an existing cache directory: the budget is enforced
/// before this run's directory is created, never after, so the install command
/// never sees `AETHYME_PREPARE_CACHE_DIR` pointing at a directory that the
/// rotation just moved away.
#[test]
fn a_cache_rotation_leaves_this_runs_cache_directory_in_place() {
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    git(temp.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(temp.path().join(".aethyme")).unwrap();
    std::fs::write(
        temp.path().join(".gitignore"),
        "/.aethyme/broker.db*\n/.aethyme/run/\n/node_modules/\n/host-state/\n",
    )
    .unwrap();
    std::fs::write(temp.path().join("pnpm-lock.yaml"), "v1\n").unwrap();
    std::fs::write(
        temp.path().join(".aethyme/prepare.toml"),
        r#"schema_version = 1

[shared_cache]
max_bytes = 1

[[steps]]
name = "deps"
command = ["sh", "-c", "test -d \"$AETHYME_PREPARE_CACHE_DIR\" && mkdir -p node_modules"]
inputs = ["pnpm-lock.yaml"]
outputs = ["node_modules/"]
cache = "repository_shared"
"#,
    )
    .unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-qm", "fixture"]);
    let state = temp.path().join("host-state");
    let adopted = success_json(run(
        temp.path(),
        &state,
        &["start", "--adopt", ".", "--task", "prepare", "--json"],
    ));
    let session = adopted["id"].as_i64().unwrap().to_string();
    success_json(run(
        temp.path(),
        &state,
        &["submit", "prepare", "--session", &session, "--json"],
    ));

    // Push the repository store over its one-byte budget, then change the
    // lockfile so the next run is a fresh digest that must rotate.
    let store = std::fs::read_dir(state.join("preparation-cache"))
        .unwrap()
        .flatten()
        .next()
        .expect("the first run created the repository store")
        .path();
    std::fs::write(store.join("old-artifact"), "bytes over budget").unwrap();
    std::fs::write(temp.path().join("pnpm-lock.yaml"), "v2\n").unwrap();
    std::fs::remove_dir_all(temp.path().join("node_modules")).unwrap();

    let rotated = success_json(run(
        temp.path(),
        &state,
        &["submit", "prepare", "--session", &session, "--json"],
    ));
    assert_eq!(rotated["state"], "current", "{rotated}");
    assert_eq!(rotated["schema_version"], 2, "{rotated}");
    assert!(
        rotated["shared_cache_rotation"].is_object(),
        "the over-budget store must be reported as rotated: {rotated}"
    );
    assert_eq!(rotated["shared_cache_rotation"]["inventory_complete"], true);
    assert_eq!(
        rotated["shared_cache_rotation"]["rotation_reason"],
        "budget_exceeded"
    );
    assert!(rotated["shared_cache_rotation"]["bytes_before"].is_number());
    assert!(!store.join("old-artifact").exists());
    assert!(temp.path().join("node_modules").is_dir());
}

#[path = "common/json_error.rs"]
mod json_error;
