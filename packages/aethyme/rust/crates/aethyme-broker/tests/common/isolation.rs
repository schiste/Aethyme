//! Owned placement for integration fixtures that intentionally retain worktrees.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::Broker;

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
    fixture_root(repo).join("worktrees")
}

/// Scope worktree allocation and subsequent broker coordination to the fixture.
/// Native test runners must also isolate host state before library startup.
pub fn open(repo: &Path) -> Broker {
    let root = fixture_root(repo);
    let broker = Broker::open(repo).unwrap();
    let key = broker.worktree_root_plan().unwrap().repository_key;
    broker
        .with_worktree_root(worktree_root(repo).join(key))
        .with_host_operation_database(root.join("host-state/host-operations.db"))
        .with_collaboration_state(root.join("host-state"))
}

pub fn command(binary: &str, repo: &Path, args: &[&str]) -> Command {
    let root = fixture_root(repo);
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(repo)
        .env("AETHYME_WORKTREE_ROOT", worktree_root(repo))
        .env("AETHYME_HOST_STATE_DIR", root.join("host-state"))
        .env("AETHYME_HOST_CACHE_DIR", root.join("host-cache"))
        .env(
            "AETHYME_CHAU7_MCP_BRIDGE",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/no-chau7-bridge"),
        );
    command
}
