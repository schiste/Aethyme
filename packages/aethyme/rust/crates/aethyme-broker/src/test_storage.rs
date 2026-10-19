//! Owned storage for library test fixtures, selected before broker startup.

use std::io;
use std::path::{Path, PathBuf};

use crate::{Broker, BrokerOpError, BrokerOpenOptions};

/// Keep this owner alive across every broker drop/reopen in one fixture.
/// Storage is a sibling of the temporary repository, never an ambient host
/// directory. Dropping a broker handle does not remove retained checkouts.
pub(crate) struct TestStorage {
    _owner: tempfile::TempDir,
    root: PathBuf,
}

impl TestStorage {
    pub(crate) fn new() -> Self {
        Self::try_new().expect("create fixture storage owner")
    }

    pub(crate) fn try_new() -> io::Result<Self> {
        let owner = tempfile::tempdir()?;
        let root = match owner.path().canonicalize() {
            Ok(root) => root,
            Err(error) => {
                crate::warn_unrecorded("remove incomplete fixture storage owner", owner.close());
                return Err(error);
            }
        };
        if let Err(error) = std::fs::create_dir(root.join("preparation-cache")) {
            crate::warn_unrecorded("remove incomplete fixture storage owner", owner.close());
            return Err(error);
        }
        Ok(Self {
            _owner: owner,
            root,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn host_state_dir(&self) -> PathBuf {
        self.root.join("host-state")
    }

    pub(crate) fn close(self) -> io::Result<()> {
        self._owner.close()
    }

    pub(crate) fn worktree_root(&self) -> PathBuf {
        self.root.join("worktrees")
    }

    pub(crate) fn options(&self) -> BrokerOpenOptions {
        let state = self.host_state_dir();
        BrokerOpenOptions::default()
            .with_host_state_dir(&state)
            .with_host_cache_dir(self.root.join("host-cache"))
            .with_host_operation_database(state.join("host-operations.db"))
            .with_worktree_root(self.worktree_root())
            .with_collaboration_state(self.root.join("collaboration"))
            .with_preparation_cache_root(self.root.join("preparation-cache"))
    }

    pub(crate) fn open(&self, repository: &Path) -> Result<Broker, BrokerOpError> {
        Broker::open_with_options(repository, self.options())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_options_bind_every_storage_domain_before_open() {
        let storage = TestStorage::new();
        let options = storage.options();
        let root = &storage.root;
        assert_eq!(options.host_state_dir, Some(root.join("host-state")));
        assert_eq!(options.host_cache_dir, Some(root.join("host-cache")));
        assert_eq!(
            options.host_operation_db_path,
            Some(root.join("host-state/host-operations.db"))
        );
        assert_eq!(options.worktree_root, Some(root.join("worktrees")));
        assert_eq!(
            options.collaboration_state_dir,
            Some(root.join("collaboration"))
        );
        assert_eq!(
            options.preparation_cache_root,
            Some(root.join("preparation-cache"))
        );
        assert!(root.join("preparation-cache").is_dir());
    }

    #[test]
    fn storage_owner_preserves_checkouts_across_broker_reopen_then_removes_them() {
        let repository = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["commit", "--allow-empty", "-qm", "fixture"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(repository.path())
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let storage = TestStorage::new();
        let storage_root = storage.root.clone();
        let mut broker = storage.open(repository.path()).unwrap();
        let session = broker.start_worktree("retained fixture", None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        assert!(worktree.starts_with(storage.worktree_root().canonicalize().unwrap()));
        std::fs::write(worktree.join("uncommitted"), b"preserve across reopen").unwrap();
        drop(broker);

        assert!(worktree.is_dir());
        let mut reopened = storage.open(repository.path()).unwrap();
        assert_eq!(
            reopened.store().session(session.id).unwrap().worktree_path,
            session.worktree_path
        );
        assert_eq!(
            std::fs::read(worktree.join("uncommitted")).unwrap(),
            b"preserve across reopen"
        );
        drop(reopened);
        drop(storage);

        assert!(!storage_root.exists());
        assert!(!worktree.exists());
        assert!(repository.path().join(".git").is_dir());
    }

    #[test]
    fn explicit_fixture_worktree_root_wins_over_hostile_child_environment() {
        const CHILD_MARKER: &str = "AETHYME_TEST_FIXTURE_STORAGE_CHILD";
        if std::env::var_os(CHILD_MARKER).is_some() {
            let storage = TestStorage::new();
            let repository = storage.root().join("repository");
            std::fs::create_dir(&repository).unwrap();
            init_repository(&repository);

            let hostile = PathBuf::from(std::env::var_os("AETHYME_WORKTREE_ROOT").unwrap());
            let mut broker = storage.open(&repository).unwrap();
            let session = broker
                .start_worktree("hostile environment fixture", None)
                .unwrap();
            let worktree = PathBuf::from(session.worktree_path);
            assert!(worktree.starts_with(storage.worktree_root().canonicalize().unwrap()));
            assert!(!worktree.starts_with(&hostile));
            assert!(!hostile.exists());
            drop(broker);
            drop(storage);
            return;
        }

        let parent = tempfile::tempdir().unwrap();
        let hostile = parent.path().join("hostile-worktrees");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("test_storage::tests::explicit_fixture_worktree_root_wins_over_hostile_child_environment")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env("AETHYME_WORKTREE_ROOT", &hostile)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child test failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!hostile.exists());
    }

    fn init_repository(repository: &Path) {
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
            vec!["commit", "--allow-empty", "-qm", "fixture"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(repository)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }
}
