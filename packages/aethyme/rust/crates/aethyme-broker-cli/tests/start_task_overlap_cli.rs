//! Lexical duplicate warnings must work before edits and without graph state.
use std::process::Command;
mod common;
const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

#[test]
fn start_names_the_existing_owner_for_task_path_overlap_without_a_graph() {
    let fixture = common::FixtureRepository::new();
    let git = || {
        let mut command = Command::new("git");
        for (name, _) in
            std::env::vars_os().filter(|(name, _)| name.to_string_lossy().starts_with("GIT_"))
        {
            command.env_remove(name);
        }
        command
            .current_dir(fixture.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        command
    };
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.name", "test"],
        vec!["config", "user.email", "test@example.com"],
    ] {
        assert!(
            git()
                .args(args)
                .current_dir(fixture.path())
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(fixture.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    assert!(
        git()
            .args(["add", ".gitignore"])
            .current_dir(fixture.path())
            .status()
            .unwrap()
            .success()
    );
    assert!(
        git()
            .args(["commit", "-qm", "fixture"])
            .current_dir(fixture.path())
            .status()
            .unwrap()
            .success()
    );
    let start = |task: &str, short: &str| {
        let output = common::broker_cli_for_fixture(
            CLI,
            &["start", "--task", task, "--short-name", short, "--json"],
            fixture.path(),
        )
        .current_dir(fixture.path())
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let first = start("Fix config/decoupling/inventory.json", "owner");
    let second = start("Regenerate config/decoupling/inventory.json", "overlap");
    assert!(!fixture.path().join(".aethyme/graph").exists());
    let duplicates = second["duplicate_work"].as_array().unwrap();
    assert!(
        duplicates
            .iter()
            .any(|row| row["session_id"] == first["id"] && row["reason"] == "task_mentions_path")
    );
    let unrelated = start("Fix src/another.rs", "unrelated");
    assert!(
        unrelated["duplicate_work"]
            .as_array()
            .is_none_or(|rows| rows.is_empty())
    );
    let status = common::broker_cli_for_fixture(CLI, &["status", "--json"], fixture.path())
        .current_dir(fixture.path())
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    let pairs: Vec<_> = status["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["id"] == "session.duplicate-work")
        .collect();
    assert_eq!(
        pairs.len(),
        1,
        "a shared path produces one canonical pair, not two"
    );
    assert_eq!(pairs[0]["session_id"], first["id"]);
    assert!(
        pairs[0]["summary"]
            .as_str()
            .unwrap()
            .contains(&format!("session {}", second["id"]))
    );
}
