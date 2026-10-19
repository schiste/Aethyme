//! Owned placement for integration fixtures that intentionally retain worktrees.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, BrokerOpenOptions};

/// Keep the repository and its external worktree container under one owner.
pub struct FixtureRepo {
    _root: tempfile::TempDir,
    repo: PathBuf,
}

impl FixtureRepo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn path(&self) -> &Path {
        &self.repo
    }
}

impl Default for FixtureRepo {
    fn default() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        Self { _root: root, repo }
    }
}

fn fixture_root(repo: &Path) -> &Path {
    assert_eq!(repo.file_name().unwrap(), "repo");
    repo.parent()
        .expect("fixture repository has an owning root")
}

pub fn worktree_root(repo: &Path) -> PathBuf {
    fixture_root(repo)
        .canonicalize()
        .expect("canonicalize fixture owner")
        .join("worktrees")
}

/// Scope storage and worktree allocation to the fixture before startup.
pub fn open(repo: &Path) -> Broker {
    let root = fixture_root(repo).canonicalize().unwrap();
    let host_state_dir = root.join("host-state");
    let preparation_cache = root.join("preparation-cache");
    std::fs::create_dir_all(&preparation_cache).unwrap();
    let options = BrokerOpenOptions::default()
        .with_host_state_dir(host_state_dir.clone())
        .with_host_cache_dir(root.join("host-cache"))
        .with_preparation_cache_root(preparation_cache)
        .with_host_operation_database(host_state_dir.join("host-operations.db"))
        .with_worktree_root(worktree_root(repo))
        .with_collaboration_state(host_state_dir.join("collaboration"));
    let broker = Broker::open_with_options(repo, options).unwrap();
    #[cfg(unix)]
    let broker = broker
        .with_agent_supervisor_executable(env!("CARGO_BIN_EXE_broker-cli-shim"))
        .unwrap();
    broker
}

pub fn command(binary: &str, repo: &Path, args: &[&str]) -> Command {
    let broker = open(repo);
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(repo)
        .env_remove("AETHYME_BROKER_DB")
        .env_remove("AETHYME_GATE_BROKER_DATABASES")
        .env_remove("AETHYME_HOST_STATE_DIR")
        .env_remove("AETHYME_HOST_CACHE_DIR")
        .env_remove("AETHYME_WORKTREE_ROOT")
        .env_remove("AETHYME_BROKER_STORAGE_CONTEXT_V1")
        .env_remove("AETHYME_BROKER_OPEN_HOST_OPERATION_DB")
        .env_remove("AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT")
        .env_remove("AETHYME_BROKER_OPEN_WORKTREE_ROOT")
        .env_remove("AETHYME_BROKER_OPEN_COLLABORATION_ROOT")
        .env(
            "AETHYME_CHAU7_MCP_BRIDGE",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/no-chau7-bridge"),
        );
    broker
        .configure_storage_context_for_child(&mut command)
        .unwrap();
    command
}
