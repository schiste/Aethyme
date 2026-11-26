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
command = ["sh", "-c", "mkdir -p .aethyme/run && touch .aethyme/run/runtime-probe-ran && echo shell"]

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
        ".aethyme/run/runtime-probe-ran",
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
    // Status reads never run a runtime probe.
    assert!(!temp.path().join(".aethyme/run/runtime-probe-ran").exists());

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
    assert!(prepared["shared_cache_wait_ms"].as_u64().is_some());
    let metrics =
        std::fs::read_to_string(temp.path().join(".aethyme/logs/command-metrics.jsonl")).unwrap();
    let metric = metrics
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value["command"] == "prepare")
        .unwrap();
    assert_eq!(metric["preparation"]["steps"], prepared["steps"]);
    assert_eq!(
        metric["preparation"]["shared_cache_wait_ms"],
        prepared["shared_cache_wait_ms"]
    );
    assert_eq!(prepared["steps"].as_array().map(Vec::len), Some(3));
    for step in prepared["steps"].as_array().unwrap() {
        assert!(step["duration_ms"].as_u64().is_some());
    }
    // A preparation runs the declared probe and keys on its output.
    assert!(temp.path().join(".aethyme/run/runtime-probe-ran").exists());
    assert_eq!(
        prepared["package_store"]["runtimes"][0]["probe_summary"],
        "shell"
    );
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
    assert_eq!(prepared["digest"], current["expected_digest"]);
    let repeated = success_json(run(
        temp.path(),
        &state,
        &["submit", "prepare", "--session", &session, "--json"],
    ));
    assert_eq!(repeated["digest"], current["expected_digest"]);
    assert_eq!(repeated["steps"].as_array().map(Vec::len), Some(0));
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

#[test]
fn prepare_metric_and_record_time_each_configured_step() {
    let temp = fixture();
    let state = temp.path().join("host-state");
    let adopted = success_json(run(
        temp.path(),
        &state,
        &["start", "--adopt", ".", "--task", "step timings", "--json"],
    ));
    let session_id = adopted["id"].as_i64().unwrap();
    let session = session_id.to_string();

    std::fs::write(
        temp.path().join(".aethyme/prepare.toml"),
        r#"schema_version = 1

[[steps]]
name = "installs"
command = ["sh", "-c", "mkdir -p node_modules"]
inputs = ["package-lock.json"]
outputs = ["node_modules/"]
cache = "repository_shared"

[[steps]]
name = "fails"
command = ["sh", "-c", "exit 3"]
inputs = ["Cargo.lock"]
outputs = ["target/"]
"#,
    )
    .unwrap();
    let failed = run(
        temp.path(),
        &state,
        &["submit", "prepare", "--session", &session, "--json"],
    );
    assert!(!failed.status.success());

    let metric = last_prepare_metric(temp.path());
    assert_eq!(
        step_summary(&metric),
        [
            (0, "installs", "repository_shared", "online", "ok"),
            (1, "fails", "worktree_local", "online", "failed"),
        ],
        "{metric}"
    );

    // The record keeps the same per-step timings next to the failed step.
    let record_path = temp.path().join(format!(
        ".aethyme/run/preparation/session-{session_id}.json"
    ));
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(record["state"], "failed");
    assert_eq!(record["failed_step"], "fails");
    assert_eq!(&record["steps"], &metric["preparation_steps"], "{record}");

    // Fixed, the second step runs for at least its sleep.
    std::fs::write(
        temp.path().join(".aethyme/prepare.toml"),
        r#"schema_version = 1

[[steps]]
name = "installs"
command = ["sh", "-c", "mkdir -p node_modules"]
inputs = ["package-lock.json"]
outputs = ["node_modules/"]
cache = "repository_shared"

[[steps]]
name = "sleeps"
command = ["sh", "-c", "sleep 0.05 && mkdir -p target"]
inputs = ["Cargo.lock"]
outputs = ["target/"]
"#,
    )
    .unwrap();
    let prepared = success_json(run(
        temp.path(),
        &state,
        &["submit", "prepare", "--session", &session, "--json"],
    ));
    assert_eq!(prepared["state"], "current", "{prepared}");
    let metric = last_prepare_metric(temp.path());
    assert_eq!(
        step_summary(&metric),
        [
            (0, "installs", "repository_shared", "online", "ok"),
            (1, "sleeps", "worktree_local", "online", "ok"),
        ],
        "{metric}"
    );
    assert!(
        metric["preparation_steps"][1]["duration_ms"]
            .as_u64()
            .is_some_and(|millis| millis >= 50),
        "{metric}"
    );
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(record["state"], "current");
    assert_eq!(&record["steps"], &metric["preparation_steps"], "{record}");

    // A cache hit runs nothing: every step is skipped, in zero time.
    let hit = success_json(run(
        temp.path(),
        &state,
        &["submit", "prepare", "--session", &session, "--json"],
    ));
    assert_eq!(hit["cache_outcome"]["hit"], true, "{hit}");
    let metric = last_prepare_metric(temp.path());
    assert_eq!(
        step_summary(&metric),
        [
            (0, "installs", "repository_shared", "skipped", "skipped"),
            (1, "sleeps", "worktree_local", "skipped", "skipped"),
        ],
        "{metric}"
    );
    for step in metric["preparation_steps"].as_array().unwrap() {
        assert_eq!(step["duration_ms"], 0, "{step}");
    }
}

/// The command metric line of the most recent `prepare`.
fn last_prepare_metric(repo: &Path) -> serde_json::Value {
    std::fs::read_to_string(repo.join(".aethyme/logs/command-metrics.jsonl"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .rfind(|value| value["command"] == "prepare")
        .unwrap()
}

/// `(index, name, cache, mode, outcome)` of each `preparation_steps` entry;
/// every entry must also carry `duration_ms`.
fn step_summary(metric: &serde_json::Value) -> Vec<(u64, &str, &str, &str, &str)> {
    let steps = metric["preparation_steps"]
        .as_array()
        .unwrap_or_else(|| panic!("no preparation_steps attachment: {metric}"));
    steps
        .iter()
        .map(|step| {
            assert!(step["duration_ms"].as_u64().is_some(), "{step}");
            (
                step["index"].as_u64().unwrap(),
                step["name"].as_str().unwrap(),
                step["cache"].as_str().unwrap(),
                step["mode"].as_str().unwrap(),
                step["outcome"].as_str().unwrap(),
            )
        })
        .collect()
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
    // A background measurement of this store waits after a deadline, and so
    // does one under another cache root.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let deadline_waits = seed_deadline_waits(temp.path(), &store);
    // The first run measured the store, so a repository that never shares
    // the cache would not measure again within the hour: forget that check.
    forget_shared_cache_budget_checks(temp.path());

    let rotated = run(
        temp.path(),
        &state,
        &["submit", "prepare", "--session", &session, "--json"],
    );
    // The drain runs under the cache lease before any step: it must say so
    // as soon as it starts instead of holding the lease in silence.
    let stderr = String::from_utf8_lossy(&rotated.stderr).into_owned();
    assert!(
        stderr.contains("preparation cache: removing the rotated shared cache")
            && stderr.contains("lease held"),
        "stderr={stderr}"
    );
    let rotated = success_json(rotated);
    assert_eq!(rotated["state"], "current", "{rotated}");
    let operations: Vec<_> = rotated["shared_cache_operations"]
        .as_array()
        .unwrap_or_else(|| panic!("the held operations are reported: {rotated}"))
        .iter()
        .map(|operation| operation["operation"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        operations,
        [
            "waiting_for_cache",
            "rotation_recovery",
            "budget_inventory",
            "rotation_drain",
            "preparation_step"
        ],
        "{rotated}"
    );
    assert_eq!(rotated["schema_version"], 2, "{rotated}");
    assert!(
        rotated["shared_cache_rotation"].is_object(),
        "the over-budget store must be reported as rotated: {rotated}"
    );
    // The inventory stops as soon as it passes the one-byte budget.
    assert_eq!(
        rotated["shared_cache_rotation"]["inventory_complete"],
        false
    );
    assert_eq!(
        rotated["shared_cache_rotation"]["rotation_reason"],
        "budget_exceeded"
    );
    assert!(rotated["shared_cache_rotation"]["bytes_before"].is_null());
    assert!(
        rotated["shared_cache_rotation"]["observed_bytes_lower_bound"]
            .as_u64()
            .unwrap()
            > 1,
        "{rotated}"
    );
    let inventory = rotated["shared_cache_operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["operation"] == "budget_inventory")
        .unwrap();
    assert_eq!(inventory["outcome"], "over_budget", "{rotated}");
    assert!(!store.join("old-artifact").exists());
    assert!(temp.path().join("node_modules").is_dir());
    // The rotated store's wait is over; the other root's is not.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let [(ended, _), (kept, record)] = &deadline_waits;
        assert_eq!(read_meta(temp.path(), ended), "null");
        assert_eq!(&read_meta(temp.path(), kept), record);
    }

    // Within the hour after a check, the next preparation skips the
    // inventory even though the store is over budget again.
    std::fs::write(store.join("old-artifact"), "bytes over budget").unwrap();
    std::fs::write(temp.path().join("pnpm-lock.yaml"), "v3\n").unwrap();
    std::fs::remove_dir_all(temp.path().join("node_modules")).unwrap();
    let skipped = success_json(run(
        temp.path(),
        &state,
        &["submit", "prepare", "--session", &session, "--json"],
    ));
    assert!(skipped.get("shared_cache_rotation").is_none(), "{skipped}");
    assert!(
        !skipped["shared_cache_operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|operation| operation["operation"] == "budget_inventory"),
        "{skipped}"
    );
    assert!(store.join("old-artifact").exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_meta(repo: &Path, key: &str) -> String {
    rusqlite::Connection::open(repo.join(".aethyme/broker.db"))
        .unwrap()
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .unwrap()
}

/// Record a measurement wait of `store` under its own cache root and under
/// another one; returns each metadata key with the record written there.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn seed_deadline_waits(repo: &Path, store: &Path) -> [(String, String); 2] {
    let db = rusqlite::Connection::open(repo.join(".aethyme/broker.db")).unwrap();
    let stamp: String = db
        .query_row(
            "SELECT key FROM meta WHERE key LIKE 'preparation.shared_cache_budget_checked_at_ms.%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let root_key = stamp.rsplit('.').next().unwrap();
    [root_key, "000000000000"].map(|root_key| {
        let key = format!("preparation.shared_cache_measurement.deadline_wait.{root_key}");
        let record = serde_json::json!({
            "repository": store.file_name().unwrap().to_str().unwrap(),
            "cache_root_key": root_key,
            "store_identity": store_identity_digest(store),
            "consecutive": 1,
            "until_ms": 1,
        })
        .to_string();
        db.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)",
            [&key, &record],
        )
        .unwrap();
        (key, record)
    })
}

/// The digest the broker names a store directory with: of its device, inode
/// and filesystem identifier.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn store_identity_digest(store: &Path) -> String {
    use sha2::{Digest, Sha256};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(store).unwrap();
    let path = std::ffi::CString::new(store.as_os_str().as_bytes()).unwrap();
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: the path is NUL-terminated and stat is writable storage.
    assert_eq!(unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) }, 0);
    // SAFETY: the successful call initialized the structure.
    let stat = unsafe { stat.assume_init() };
    let fsid = std::ptr::addr_of!(stat.f_fsid).cast::<u8>();
    // SAFETY: fsid points to the live f_fsid field, read for its exact size.
    let bytes = unsafe { std::slice::from_raw_parts(fsid, std::mem::size_of_val(&stat.f_fsid)) };
    let filesystem_id: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let identity = format!(
        r#"{{"device":{},"inode":{},"filesystem_id":"{filesystem_id}"}}"#,
        metadata.dev(),
        metadata.ino()
    );
    format!("{:x}", Sha256::digest(identity.as_bytes()))[..32].to_string()
}

/// Remove the record of when each shared-cache budget check last ran.
fn forget_shared_cache_budget_checks(repo: &Path) {
    let db = rusqlite::Connection::open(repo.join(".aethyme/broker.db")).unwrap();
    let removed = db
        .execute(
            "DELETE FROM meta WHERE key LIKE 'preparation.shared_cache_budget_checked_at_ms.%'",
            [],
        )
        .unwrap();
    assert_eq!(
        removed, 1,
        "the first preparation recorded its budget check"
    );
}

#[path = "common/json_error.rs"]
mod json_error;

/// Two sessions preparing at once is ordinary contention on the host-wide
/// shared cache. The second one waits by default; `--wait 0s` still refuses.
#[test]
fn a_contended_shared_cache_is_waited_for_by_default() {
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    git(temp.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(temp.path().join(".aethyme")).unwrap();
    std::fs::write(
        temp.path().join(".gitignore"),
        "/.aethyme/broker.db*\n/.aethyme/run/\n/node_modules/\n/host-state/\n/step-started\n",
    )
    .unwrap();
    std::fs::write(temp.path().join("pnpm-lock.yaml"), "v1\n").unwrap();
    // The first session holds the shared cache for as long as `release`
    // is absent, so the test controls the contention window exactly.
    let release = temp.path().join("host-state").join("release");
    std::fs::write(
        temp.path().join(".aethyme/prepare.toml"),
        format!(
            r#"schema_version = 1

[[steps]]
name = "deps"
command = ["sh", "-c", "touch step-started; while [ ! -e '{}' ] && [ -e hold ]; do sleep 0.05; done; mkdir -p node_modules"]
inputs = ["pnpm-lock.yaml"]
outputs = ["node_modules/"]
cache = "repository_shared"
"#,
            release.display()
        ),
    )
    .unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-qm", "fixture"]);
    let state = temp.path().join("host-state");
    let holder = success_json(run(
        temp.path(),
        &state,
        &["start", "--adopt", ".", "--task", "holder", "--json"],
    ));
    let holder_id = holder["id"].as_i64().unwrap().to_string();
    let waiter = success_json(run(
        temp.path(),
        &state,
        &[
            "start",
            "--task",
            "waiter",
            "--short-name",
            "waiter",
            "--json",
        ],
    ));
    let waiter_id = waiter["id"].as_i64().unwrap().to_string();
    // Only the holder's checkout blocks in its step.
    std::fs::write(temp.path().join("hold"), "").unwrap();

    let holding = {
        let (repo, state, holder_id) =
            (temp.path().to_path_buf(), state.clone(), holder_id.clone());
        std::thread::spawn(move || {
            run(
                &repo,
                &state,
                &["submit", "prepare", "--session", &holder_id, "--json"],
            )
        })
    };
    let started = std::time::Instant::now();
    while !temp.path().join("step-started").exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the holder never started its shared step"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // Another session can read what the holder is doing.
    let holder_status = success_json(run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "status",
            "--session",
            &holder_id,
            "--json",
        ],
    ));
    assert_eq!(
        holder_status["current_operation"]["operation"], "preparation_step",
        "{holder_status}"
    );
    assert_eq!(
        holder_status["current_operation"]["detail"], "deps",
        "{holder_status}"
    );
    let waiter_status = success_json(run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "status",
            "--session",
            &waiter_id,
            "--json",
        ],
    ));
    assert!(
        waiter_status.get("current_operation").is_none(),
        "{waiter_status}"
    );

    let refused = run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "--session",
            &waiter_id,
            "--wait",
            "0s",
            "--json",
        ],
    );
    assert!(!refused.status.success(), "--wait 0s must still fail fast");
    // Contention is a host condition to wait out, not an unclassified failure.
    assert_eq!(refused.status.code(), Some(6), "{refused:?}");
    // `--json` reports the refusal on stdout; the human form uses stderr.
    let refusal = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stdout),
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(refusal.contains("preparation_cache"), "{refusal}");

    let waiting = {
        let (repo, state, waiter_id) =
            (temp.path().to_path_buf(), state.clone(), waiter_id.clone());
        std::thread::spawn(move || {
            run(
                &repo,
                &state,
                &["submit", "prepare", "--session", &waiter_id, "--json"],
            )
        })
    };
    // Long enough for the waiter to reach the held cache, and for an
    // immediate refusal to have returned.
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(
        !waiting.is_finished(),
        "prepare without --wait must wait for the shared cache, not refuse"
    );
    // The waiting session says what it waits on.
    let waiting_status = success_json(run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "status",
            "--session",
            &waiter_id,
            "--json",
        ],
    ));
    assert_eq!(
        waiting_status["current_operation"]["operation"], "waiting_for_cache",
        "{waiting_status}"
    );
    assert_eq!(
        waiting_status["waiting_on"]["session_id"].to_string(),
        holder_id,
        "{waiting_status}"
    );
    assert_eq!(
        waiting_status["waiting_on"]["current_operation"]["detail"], "deps",
        "{waiting_status}"
    );
    std::fs::write(&release, "").unwrap();

    success_json(holding.join().unwrap());
    let waited = waiting.join().unwrap();
    let waited_stderr = String::from_utf8_lossy(&waited.stderr).into_owned();
    assert!(
        waited_stderr.contains("waiting up to 600s"),
        "stderr={waited_stderr}"
    );
    // The waiter names what the holder is doing, not only that it waits.
    assert!(
        waited_stderr.contains(&format!("the holder, session {holder_id}"))
            && waited_stderr.contains("running preparation step \"deps\""),
        "stderr={waited_stderr}"
    );
    let waited = success_json(waited);
    assert_eq!(waited["state"], "current", "{waited}");
    assert!(
        waited["shared_cache_wait_ms"].as_u64().unwrap() >= 500,
        "{waited}"
    );
}

/// With `max_concurrent`, a second session prepares while the first is still
/// inside its shared step, instead of waiting for it.
#[test]
fn a_shared_cache_admits_concurrent_preparations_when_the_repository_opts_in() {
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    git(temp.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(temp.path().join(".aethyme")).unwrap();
    std::fs::write(
        temp.path().join(".gitignore"),
        "/.aethyme/broker.db*\n/.aethyme/run/\n/node_modules/\n/host-state/\n/step-started\n/hold\n",
    )
    .unwrap();
    std::fs::write(temp.path().join("pnpm-lock.yaml"), "v1\n").unwrap();
    let release = temp.path().join("host-state").join("release");
    std::fs::write(
        temp.path().join(".aethyme/prepare.toml"),
        format!(
            r#"schema_version = 1

[[steps]]
name = "deps"
command = ["sh", "-c", "touch step-started; while [ ! -e '{}' ] && [ -e hold ]; do sleep 0.05; done; mkdir -p node_modules"]
inputs = ["pnpm-lock.yaml"]
outputs = ["node_modules/"]
cache = "repository_shared"

[shared_cache]
max_concurrent = 2
"#,
            release.display()
        ),
    )
    .unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-qm", "fixture"]);
    let state = temp.path().join("host-state");
    // This test exercises shared-cache concurrency, so the independent host
    // install pool needs two slots even when warning pressure halves its limit.
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("preparation.toml"),
        "[installs]\nmax_concurrent = 4\n",
    )
    .unwrap();
    let start = |task: &str| {
        let started = success_json(run(
            temp.path(),
            &state,
            &["start", "--task", task, "--short-name", task, "--json"],
        ));
        started["id"].as_i64().unwrap().to_string()
    };
    let holder = success_json(run(
        temp.path(),
        &state,
        &["start", "--adopt", ".", "--task", "holder", "--json"],
    ));
    let holder_id = holder["id"].as_i64().unwrap().to_string();
    let (first_id, second_id) = (start("first"), start("second"));

    // The first preparation finds a free cache whose budget was never
    // checked, so it holds the cache alone and checks it.
    let first = success_json(run(
        temp.path(),
        &state,
        &["submit", "prepare", "--session", &first_id, "--json"],
    ));
    assert_eq!(first["shared_cache_mode"], "exclusive", "{first}");

    std::fs::write(temp.path().join("hold"), "").unwrap();
    let holding = {
        let (repo, state, holder_id) = (temp.path().to_path_buf(), state.clone(), holder_id);
        std::thread::spawn(move || {
            run(
                &repo,
                &state,
                &["submit", "prepare", "--session", &holder_id, "--json"],
            )
        })
    };
    let started = std::time::Instant::now();
    while !temp.path().join("step-started").exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the holder never started its shared step"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // The holder is still in its step: `--wait 0s` proves nothing waited.
    let alongside = success_json(run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "--session",
            &second_id,
            "--wait",
            "0s",
            "--json",
        ],
    ));
    assert_eq!(alongside["state"], "current", "{alongside}");
    assert_eq!(alongside["shared_cache_mode"], "shared", "{alongside}");
    assert!(!holding.is_finished(), "the holder must still be preparing");

    std::fs::write(&release, "").unwrap();
    let held = success_json(holding.join().unwrap());
    assert_eq!(held["shared_cache_mode"], "shared", "{held}");
}

/// A sharing preparation that takes the cache alone for its budget check
/// trades it for a share before its steps: a second session prepares while
/// the first is still inside its step, instead of waiting for its install.
#[test]
fn a_budget_check_holds_the_cache_alone_only_until_its_steps_start() {
    let temp = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    git(temp.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(temp.path().join(".aethyme")).unwrap();
    std::fs::write(
        temp.path().join(".gitignore"),
        "/.aethyme/broker.db*\n/.aethyme/run/\n/.aethyme/logs/\n/node_modules/\n/host-state/\n/step-started\n/hold\n",
    )
    .unwrap();
    std::fs::write(temp.path().join("pnpm-lock.yaml"), "v1\n").unwrap();
    let release = temp.path().join("host-state").join("release");
    std::fs::write(
        temp.path().join(".aethyme/prepare.toml"),
        format!(
            r#"schema_version = 1

[[steps]]
name = "deps"
command = ["sh", "-c", "touch step-started; while [ ! -e '{}' ] && [ -e hold ]; do sleep 0.05; done; mkdir -p node_modules"]
inputs = ["pnpm-lock.yaml"]
outputs = ["node_modules/"]
cache = "repository_shared"

[shared_cache]
max_concurrent = 2
"#,
            release.display()
        ),
    )
    .unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-qm", "fixture"]);
    let state = temp.path().join("host-state");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(
        state.join("preparation.toml"),
        "[installs]\nmax_concurrent = 4\n",
    )
    .unwrap();
    let holder = success_json(run(
        temp.path(),
        &state,
        &["start", "--adopt", ".", "--task", "holder", "--json"],
    ));
    let holder_id = holder["id"].as_i64().unwrap().to_string();
    let second = success_json(run(
        temp.path(),
        &state,
        &[
            "start",
            "--task",
            "second",
            "--short-name",
            "second",
            "--json",
        ],
    ));
    let second_id = second["id"].as_i64().unwrap().to_string();

    // The cache's budget was never checked, so the holder takes it alone to
    // check it, then holds its step open. `--wait 0s`: trading the cache
    // for a share must not spend a wait budget the holder does not have.
    std::fs::write(temp.path().join("hold"), "").unwrap();
    let holding = {
        let (repo, state, holder_id) = (temp.path().to_path_buf(), state.clone(), holder_id);
        std::thread::spawn(move || {
            run(
                &repo,
                &state,
                &[
                    "submit",
                    "prepare",
                    "--session",
                    &holder_id,
                    "--wait",
                    "0s",
                    "--json",
                ],
            )
        })
    };
    let started = std::time::Instant::now();
    while !temp.path().join("step-started").exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the holder never started its step"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // `--wait 0s` proves the second preparation did not wait for the step.
    let alongside = success_json(run(
        temp.path(),
        &state,
        &[
            "submit",
            "prepare",
            "--session",
            &second_id,
            "--wait",
            "0s",
            "--json",
        ],
    ));
    assert_eq!(alongside["state"], "current", "{alongside}");
    assert_eq!(alongside["shared_cache_mode"], "shared", "{alongside}");
    assert!(!holding.is_finished(), "the holder must still be preparing");

    std::fs::write(&release, "").unwrap();
    let held = success_json(holding.join().unwrap());
    assert_eq!(held["state"], "current", "{held}");
    assert_eq!(held["shared_cache_mode"], "exclusive", "{held}");
    let metrics =
        std::fs::read_to_string(temp.path().join(".aethyme/logs/command-metrics.jsonl")).unwrap();
    let downgraded = metrics
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| value["command"] == "prepare")
        .any(|value| {
            value["phase_counts"]["shared_cache_downgraded_to_shared"] == 1
                && value["phase_counts"]["shared_cache_downgrade_wait_floor"] == 1
        });
    assert!(downgraded, "{metrics}");
}
