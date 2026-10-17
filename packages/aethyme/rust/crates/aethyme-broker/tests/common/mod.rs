use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, BrokerOpError, BrokerOpenOptions};

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
        .env_remove("AETHYME_WORKTREE_ROOT")
        .env_remove("AETHYME_BROKER_STORAGE_CONTEXT_V1")
        .env_remove("AETHYME_BROKER_OPEN_HOST_OPERATION_DB")
        .env_remove("AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT")
        .env_remove("AETHYME_BROKER_OPEN_WORKTREE_ROOT")
        .env_remove("AETHYME_BROKER_OPEN_COLLABORATION_ROOT");
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

#[allow(dead_code)]
pub fn fixture_host_cache_dir(fixture_root: &Path) -> PathBuf {
    fixture_git_common_dir(fixture_root)
        .map(|git_dir| git_dir.join("aethyme-test-cache"))
        .unwrap_or_else(|| fixture_root.join("host-cache"))
}

/// Set owned host paths after constructing a CLI command. These explicit
/// paths take precedence over inherited XDG and user host-state locations.
#[allow(dead_code)]
pub fn isolate_host_paths(command: &mut Command, fixture_root: &Path) {
    command
        .env_remove("AETHYME_BROKER_DB")
        .env_remove("AETHYME_GATE_BROKER_DATABASES")
        .env_remove("AETHYME_WORKTREE_ROOT")
        .env_remove("AETHYME_BROKER_STORAGE_CONTEXT_V1")
        .env_remove("AETHYME_BROKER_OPEN_HOST_OPERATION_DB")
        .env_remove("AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT")
        .env_remove("AETHYME_BROKER_OPEN_WORKTREE_ROOT")
        .env_remove("AETHYME_BROKER_OPEN_COLLABORATION_ROOT")
        .env(
            "AETHYME_HOST_STATE_DIR",
            fixture_host_state_dir(fixture_root),
        )
        .env(
            "AETHYME_HOST_CACHE_DIR",
            fixture_host_cache_dir(fixture_root),
        );
}

#[allow(dead_code)]
pub fn broker_cli_for_fixture(binary: &str, args: &[&str], fixture_root: &Path) -> Command {
    let mut command = broker_cli(binary, args);
    isolate_host_paths(&mut command, fixture_root);
    command
}

/// A temporary repository and the external paths owned by one test fixture.
/// Keep this value alive while dropping and reopening its broker: worktrees
/// belong to the fixture, not to an individual `Broker` handle.
#[allow(dead_code)]
pub struct FixtureRepository {
    owner: tempfile::TempDir,
    repository: PathBuf,
    _repository_owner: Option<tempfile::TempDir>,
}

#[allow(dead_code)]
impl FixtureRepository {
    pub fn new() -> Self {
        let owner = tempfile::tempdir().expect("create fixture owner");
        let repository = owner.path().join("repo");
        std::fs::create_dir_all(&repository).expect("create fixture repository");
        Self {
            owner,
            repository,
            _repository_owner: None,
        }
    }

    /// Adopt an existing temporary directory as the repository while keeping
    /// broker worktrees and host state under a separate fixture-owned root.
    pub fn from_tempdir(repository_owner: tempfile::TempDir) -> Self {
        let repository = repository_owner.path().to_path_buf();
        let owner = tempfile::tempdir().expect("create fixture storage owner");
        Self {
            owner,
            repository,
            _repository_owner: Some(repository_owner),
        }
    }

    pub fn path(&self) -> &Path {
        &self.repository
    }

    pub fn owner_path(&self) -> &Path {
        self.owner.path()
    }

    pub fn worktree_root(&self) -> PathBuf {
        self.owner.path().join("worktrees")
    }
}

/// A library broker configured to use paths owned by its caller's fixture.
/// The caller must keep the fixture owner alive for the full test scope,
/// including across broker drop/reopen cycles.
#[allow(dead_code)]
pub struct FixtureBroker {
    broker: Broker,
    worktree_root: PathBuf,
}

#[allow(dead_code)]
impl FixtureBroker {
    /// Consume the wrapper when a helper's established return type is `Broker`.
    /// The fixture owner remains with the caller and must stay alive across
    /// every broker handle opened against these paths.
    pub fn into_broker(self) -> Broker {
        self.broker
    }

    /// The stable worktree root selected for this broker.
    #[allow(dead_code)]
    pub fn worktree_root(&self) -> &Path {
        &self.worktree_root
    }

    /// Select a caller-owned fixture root. The fixture owner must outlive every
    /// broker handle opened against it, not just this handle.
    pub fn with_worktree_root(mut self, root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        self.broker = self.broker.with_worktree_root(root.clone());
        self.worktree_root = root;
        self
    }
}

impl Deref for FixtureBroker {
    type Target = Broker;

    fn deref(&self) -> &Self::Target {
        &self.broker
    }
}

impl DerefMut for FixtureBroker {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.broker
    }
}

#[allow(dead_code)]
pub fn open_fixture_broker(
    repository: &Path,
    fixture_root: &Path,
) -> Result<FixtureBroker, BrokerOpError> {
    let worktree_root_path = fixture_root.join("worktrees");
    let host_state_dir = fixture_host_state_dir(fixture_root);
    let options = BrokerOpenOptions::default()
        .with_host_state_dir(host_state_dir.clone())
        .with_host_cache_dir(fixture_host_cache_dir(fixture_root))
        .with_host_operation_database(host_state_dir.join("host-operations.db"))
        .with_worktree_root(worktree_root_path.clone())
        .with_collaboration_state(host_state_dir.join("collaboration"));
    let broker = Broker::open_with_options(repository, options)?;
    Ok(FixtureBroker {
        broker,
        worktree_root: worktree_root_path,
    })
}

#[allow(dead_code)]
pub fn open_fixture_repository_broker(
    fixture: &FixtureRepository,
) -> Result<FixtureBroker, BrokerOpError> {
    open_fixture_broker(fixture.path(), fixture.owner_path())
}

/// Open a repository whose owner is its immediate parent directory. The
/// caller must retain that owner for the lifetime of this broker and any
/// reopened handles.
#[allow(dead_code)]
pub fn open_fixture_repository_path_broker(
    repository: &Path,
) -> Result<FixtureBroker, BrokerOpError> {
    let owner = repository
        .parent()
        .expect("fixture repository has an owning parent directory");
    open_fixture_broker(repository, owner)
}
