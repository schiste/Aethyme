use std::path::Path;
use std::process::{Command, Output};

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

fn run(repo: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
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
    let temp = tempfile::tempdir().unwrap();
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
fn preparation_is_explicit_language_neutral_and_invalidates_on_input_change() {
    let temp = fixture();
    let state = temp.path().join("host-state");
    let adopted = success_json(run(
        temp.path(),
        &state,
        &["adopt", ".", "--task", "prepare", "--json"],
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
        &["prepare", "status", "--session", &session, "--json"],
    ));
    assert_eq!(required["state"], "required");
    assert_eq!(
        required["next_action"],
        format!("aethyme broker prepare --session {session}")
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
        error.contains(&format!("aethyme broker prepare --session {session}")),
        "{error}"
    );

    let prepared = success_json(run(
        temp.path(),
        &state,
        &["prepare", "--session", &session, "--json"],
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
        &["prepare", "status", "--session", &session, "--json"],
    ));
    assert_eq!(current["state"], "current");
    std::fs::write(temp.path().join("Cargo.lock"), "changed bytes\n").unwrap();
    let stale = success_json(run(
        temp.path(),
        &state,
        &["prepare", "status", "--session", &session, "--json"],
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
        &["adopt", ".", "--task", "offline", "--json"],
    ));
    let session = adopted["id"].as_i64().unwrap().to_string();
    let prepared = success_json(run(
        temp.path(),
        &state,
        &["prepare", "--session", &session, "--offline", "--json"],
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
        &["adopt", ".", "--task", "failure", "--json"],
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
        &["prepare", "--session", &session, "--json"],
    );
    assert!(!failed.status.success());
    let status = success_json(run(
        temp.path(),
        &state,
        &["prepare", "status", "--session", &session, "--json"],
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
        &["prepare", "status", "--session", &session, "--json"],
    ));
    assert_eq!(interrupted["state"], "in_progress");
    let reason = interrupted["reason"].as_str().unwrap();
    assert!(reason.contains("lease-evidence"), "{reason}");
    assert!(reason.contains("generation 3"), "{reason}");
}
