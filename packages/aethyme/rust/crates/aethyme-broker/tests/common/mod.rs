use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, BrokerOpError};

/// Run the broker CLI without reaching the developer's live Chau7 instance.
/// Existing lifecycle fixtures get a stable test label unless a case is
/// explicitly testing the short-name contract.
pub fn test_session_args(args: &[&str]) -> Vec<String> {
    let mut args = args.to_vec();
    if matches!(args.first(), Some(&"start" | &"adopt" | &"start-agent"))
        && args.contains(&"--task")
        && !args.contains(&"--short-name")
    {
        args.extend(["--short-name", "test session"]);
    }
    let adopts_main = matches!(args.first(), Some(&"adopt"))
        || (args.first() == Some(&"start") && args.contains(&"--adopt"));
    if adopts_main && !args.contains(&"--allow-main-checkout") {
        // Test fixtures intentionally register sessions in their temporary
        // main checkout; exercise the explicit operator-only CLI flag.
        args.push("--allow-main-checkout");
    }
    args.into_iter().map(str::to_string).collect()
}

pub fn disabled_bridge_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/no-chau7-bridge")
}

#[allow(dead_code)]
pub fn broker_cli(binary: &str, args: &[&str]) -> Command {
    let mut command = Command::new(binary);
    command
        .args(test_session_args(args))
        .env("AETHYME_CHAU7_MCP_BRIDGE", disabled_bridge_path())
        // Test children must not inherit the developer's shared host DB,
        // cache, gate database scope, or placement root. Fixture-specific tests
        // set owned paths explicitly after calling this helper.
        .env_remove("AETHYME_BROKER_DB")
        .env_remove("AETHYME_GATE_BROKER_DATABASES")
        .env_remove("AETHYME_HOST_STATE_DIR")
        .env_remove("AETHYME_HOST_CACHE_DIR")
        .env_remove("AETHYME_WORKTREE_ROOT");
    command
}

fn fixture_git_common_dir(fixture_root: &Path) -> Option<PathBuf> {
    let git_entry = fixture_root.join(".git");
    if git_entry.is_dir() {
        return Some(git_entry);
    }
    let git_file = std::fs::read_to_string(git_entry).ok()?;
    let git_dir = git_file.trim().strip_prefix("gitdir:")?.trim();
    let git_dir = PathBuf::from(git_dir);
    let git_dir = if git_dir.is_absolute() {
        git_dir
    } else {
        fixture_root.join(git_dir)
    };
    let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(path) => {
            let path = PathBuf::from(path.trim());
            if path.is_absolute() {
                path
            } else {
                git_dir.join(path)
            }
        }
        Err(_) => git_dir,
    };
    Some(common_dir)
}

/// Host state for a fixture stays in its private Git metadata directory.
/// Non-repository fixture roots get a private child directory instead.
pub fn fixture_host_state_dir(fixture_root: &Path) -> PathBuf {
    fixture_git_common_dir(fixture_root)
        .map(|git_dir| git_dir.join("aethyme-test-state"))
        .unwrap_or_else(|| fixture_root.join("host-state"))
}

pub fn fixture_host_cache_dir(fixture_root: &Path) -> PathBuf {
    fixture_git_common_dir(fixture_root)
        .map(|git_dir| git_dir.join("aethyme-test-cache"))
        .unwrap_or_else(|| fixture_root.join("host-cache"))
}

/// Set owned host paths after constructing a CLI command. These explicit
/// paths take precedence over inherited XDG and user host-state locations.
pub fn isolate_host_paths(command: &mut Command, fixture_root: &Path) {
    command
        .env_remove("AETHYME_BROKER_DB")
        .env_remove("AETHYME_GATE_BROKER_DATABASES")
        .env_remove("AETHYME_WORKTREE_ROOT")
        .env(
            "AETHYME_HOST_STATE_DIR",
            fixture_host_state_dir(fixture_root),
        )
        .env(
            "AETHYME_HOST_CACHE_DIR",
            fixture_host_cache_dir(fixture_root),
        );
}

pub fn broker_cli_for_fixture(binary: &str, args: &[&str], fixture_root: &Path) -> Command {
    let mut command = broker_cli(binary, args);
    isolate_host_paths(&mut command, fixture_root);
    command
}

pub fn open_fixture_broker(
    repository: &Path,
    fixture_root: &Path,
) -> Result<Broker, BrokerOpError> {
    Broker::open(repository).map(|broker| {
        broker.with_host_operation_database(
            fixture_host_state_dir(fixture_root).join("host-operations.db"),
        )
    })
}
