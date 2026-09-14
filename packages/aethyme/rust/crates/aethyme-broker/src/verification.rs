//! Stable disposable checkouts for exact-tree verification.
//!
//! A slot is a checkout, and a checkout nested inside another checkout cannot
//! be isolated from ancestor-based discovery. Anything that walks upward for a
//! workspace manifest -- the ordinary idiom, and the one this repository's own
//! test helpers use -- resolves to the *enclosing* repository whenever the slot
//! is absent or incomplete, and then answers confidently with someone else's
//! tree instead of failing. Outside the repository the same walk terminates
//! with nothing, which is loud (#149).
//!
//! So a slot is placed in host-scoped storage, beside the broker worktree root
//! and under the same policy: an ephemeral repository is kept out of durable
//! host state unless that directory was named explicitly, and falls back to the
//! system temporary directory. The repository itself is the placement of last
//! resort, taken only when every location outside it is unwritable -- a sandbox
//! confining writes to the invoking checkout is the case that requires it. That
//! placement is recorded rather than silently accepted, and `gate doctor`
//! reports it.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use crate::{BrokerError, BrokerOpError, GitRepo};

/// Where a slot was placed, and what it had to settle for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlotPlacement {
    pub(crate) directory: PathBuf,
    /// True when nothing outside the repository could be written.
    pub(crate) inside_repository: bool,
    /// The location that was wanted, when it is not the one in use.
    pub(crate) preferred: Option<PathBuf>,
    /// Why each location outside the repository was rejected, in order.
    pub(crate) refusals: Vec<String>,
}

/// Choose the directory this repository's slot will occupy.
pub(crate) fn plan_slot_placement(main_root: &Path, namespace: &str) -> SlotPlacement {
    plan_slot_placement_in(
        main_root,
        namespace,
        durable_host_state(main_root).as_deref(),
        &std::env::temp_dir(),
    )
}

/// The host state directory, when this repository is entitled to durable space.
///
/// The same rule the broker worktree root applies: a repository under the
/// system temporary directory is a fixture or a scratch clone, and must not
/// leave storage in durable host state that outlives the records owning it.
fn durable_host_state(main_root: &Path) -> Option<PathBuf> {
    crate::host_state::default_host_state_dir().filter(|_| {
        crate::host_state::host_state_dir_is_explicit()
            || !crate::host_state::path_is_ephemeral(main_root)
    })
}

/// [`plan_slot_placement`] with its two environment inputs supplied.
///
/// Creating the directory *is* the writability test, and it is the same
/// directory the slot would use anyway, so planning leaves nothing behind that
/// acquiring would not have created. Callers that only want to know where a
/// slot would land -- the gate doctor -- can therefore ask without running one.
fn plan_slot_placement_in(
    main_root: &Path,
    namespace: &str,
    host_state: Option<&Path>,
    temp_root: &Path,
) -> SlotPlacement {
    let key = crate::host_state::repository_key(
        main_root,
        GitRepo::discover(main_root)
            .ok()
            .and_then(|repo| repo.git_common_dir().ok())
            .as_deref(),
    );
    let mut candidates = Vec::new();
    if let Some(base) = host_state {
        candidates.push(base.join("run").join(&key).join(namespace));
    }
    candidates.push(temp_root.join("aethyme-run").join(&key).join(namespace));
    // An explicitly named host state directory can be anywhere, including
    // under the checkout it serves, and a temporary directory can be
    // redirected the same way. Either would reintroduce the nesting that this
    // placement exists to avoid, so neither is taken on faith.
    candidates.retain(|candidate| !path_is_inside(main_root, candidate));

    let mut refusals = Vec::new();
    let mut preferred = None;
    for directory in candidates {
        preferred.get_or_insert_with(|| directory.clone());
        match std::fs::create_dir_all(&directory) {
            Ok(()) => {
                return SlotPlacement {
                    directory,
                    inside_repository: false,
                    preferred: None,
                    refusals,
                };
            }
            Err(source) => refusals.push(format!("{}: {source}", directory.display())),
        }
    }
    SlotPlacement {
        directory: main_root.join(".aethyme/run").join(namespace),
        inside_repository: true,
        preferred,
        refusals,
    }
}

/// True when `candidate` resolves inside `root`.
fn path_is_inside(root: &Path, candidate: &Path) -> bool {
    resolve(candidate).starts_with(resolve(root))
}

/// Resolve symlinks as far as the path exists, keeping the rest verbatim.
///
/// A slot directory is named before it is created, so plain `canonicalize`
/// answers `Err` for exactly the paths this has to compare -- and comparing a
/// raw candidate against a canonicalized root silently never matches, which on
/// macOS (`/var` -> `/private/var`) is every temporary checkout. Resolving the
/// existing prefix makes both sides speak the same names.
fn resolve(path: &Path) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => resolve(parent).join(name),
        _ => path.to_path_buf(),
    }
}

/// One repository-scoped checkout serialized by an advisory file lock.
///
/// A stable path lets build tools reuse safe path-sensitive fingerprints.
/// Callers choose a distinct namespace when their verification lifetimes must
/// not contend with one another.
/// The file lock covers checkout materialization, gate execution, and cleanup.
pub(crate) struct ExactTreeVerificationSlot {
    repository_root: PathBuf,
    path: PathBuf,
    _lock: File,
}

impl ExactTreeVerificationSlot {
    pub(crate) fn acquire(main_root: &Path, namespace: &str) -> Result<Self, BrokerOpError> {
        Self::acquire_at(main_root, plan_slot_placement(main_root, namespace))
    }

    /// Take the lock on an already chosen placement.
    fn acquire_at(main_root: &Path, placement: SlotPlacement) -> Result<Self, BrokerOpError> {
        std::fs::create_dir_all(&placement.directory).map_err(|source| BrokerError::Io {
            path: placement.directory.clone(),
            source,
        })?;
        let lock_path = placement.directory.join("slot.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| BrokerError::Io {
                path: lock_path.clone(),
                source,
            })?;
        // Contention blocks rather than failing, so a lock error is a broken
        // lock and never a busy one. Answering it by moving to a different
        // directory would hand one slot to two processes, so it is fatal.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(BrokerError::Io {
                path: lock_path,
                source: std::io::Error::last_os_error(),
            }
            .into());
        }
        Ok(Self {
            repository_root: main_root.to_path_buf(),
            path: placement.directory.join("slot"),
            _lock: lock,
        })
    }

    pub(crate) fn materialize(
        &mut self,
        repository: &GitRepo,
        commit: &str,
    ) -> Result<GitRepo, BrokerOpError> {
        self.cleanup();
        Ok(repository.worktree_add_detached(&self.path, commit)?)
    }

    pub(crate) fn cleanup(&mut self) {
        // Always try Git-level removal. After a crash, the common Git
        // directory can retain a registration created by another process.
        if let Ok(repository) = GitRepo::discover(&self.repository_root) {
            let _ = repository.worktree_remove(&self.path, true);
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl Drop for ExactTreeVerificationSlot {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A checkout directory, and the two storage roots the placement consults.
    struct Fixture {
        _root: tempfile::TempDir,
        main: PathBuf,
        host: PathBuf,
        temp: PathBuf,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("repo");
        std::fs::create_dir_all(&main).unwrap();
        Fixture {
            main,
            host: root.path().join("host"),
            temp: root.path().join("temp"),
            _root: root,
        }
    }

    #[test]
    fn a_slot_is_placed_outside_the_repository_it_verifies() {
        let fx = fixture();
        let placement = plan_slot_placement_in(&fx.main, "merge-sim", Some(&fx.host), &fx.temp);
        assert!(
            !placement.inside_repository,
            "placed at {}",
            placement.directory.display()
        );
        assert!(
            !path_is_inside(&fx.main, &placement.directory),
            "{} is under {}",
            placement.directory.display(),
            fx.main.display()
        );
        assert!(placement.directory.starts_with(&fx.host));
        assert!(placement.directory.ends_with("merge-sim"));
        assert_eq!(placement.preferred, None);
        assert!(placement.refusals.is_empty(), "{:?}", placement.refusals);
    }

    #[test]
    fn a_repository_without_durable_host_state_lands_in_the_temporary_root() {
        let fx = fixture();
        let placement = plan_slot_placement_in(&fx.main, "merge-sim", None, &fx.temp);
        assert!(!placement.inside_repository);
        assert!(placement.directory.starts_with(fx.temp.join("aethyme-run")));
        assert!(placement.directory.ends_with("merge-sim"));
    }

    #[test]
    fn a_host_state_directory_inside_the_repository_is_not_used() {
        let fx = fixture();
        let inside = fx.main.join(".host");
        let placement = plan_slot_placement_in(&fx.main, "merge-sim", Some(&inside), &fx.temp);
        assert!(!placement.inside_repository);
        assert!(!placement.directory.starts_with(&inside));
        assert!(placement.directory.starts_with(fx.temp.join("aethyme-run")));
    }

    #[test]
    fn every_outside_location_being_unwritable_falls_back_into_the_repository() {
        let fx = fixture();
        // A file where a directory must go is the portable way to make a
        // location unwritable without changing process privileges.
        std::fs::write(&fx.host, "not a directory").unwrap();
        std::fs::write(&fx.temp, "not a directory").unwrap();
        let placement = plan_slot_placement_in(&fx.main, "merge-sim", Some(&fx.host), &fx.temp);
        assert!(placement.inside_repository);
        assert_eq!(placement.directory, fx.main.join(".aethyme/run/merge-sim"));
        let preferred = placement.preferred.expect("the wanted location is named");
        assert!(preferred.starts_with(&fx.host), "{}", preferred.display());
        assert!(preferred.ends_with("merge-sim"));
        assert_eq!(placement.refusals.len(), 2, "{:?}", placement.refusals);
        assert!(
            placement
                .refusals
                .iter()
                .all(|refusal| refusal.contains("merge-sim")),
            "{:?}",
            placement.refusals
        );
    }

    #[test]
    fn a_writable_temporary_root_is_preferred_over_the_repository() {
        let fx = fixture();
        std::fs::write(&fx.host, "not a directory").unwrap();
        let placement = plan_slot_placement_in(&fx.main, "merge-sim", Some(&fx.host), &fx.temp);
        assert!(!placement.inside_repository);
        assert!(placement.directory.starts_with(fx.temp.join("aethyme-run")));
        assert_eq!(placement.refusals.len(), 1, "{:?}", placement.refusals);
    }

    #[test]
    fn two_namespaces_never_share_a_directory() {
        let fx = fixture();
        assert_ne!(
            plan_slot_placement_in(&fx.main, "merge-sim", Some(&fx.host), &fx.temp).directory,
            plan_slot_placement_in(&fx.main, "graph-integrity", Some(&fx.host), &fx.temp).directory
        );
    }

    #[test]
    fn slot_reuses_one_path_and_removes_stale_contents() {
        let fx = fixture();
        let plan = || plan_slot_placement_in(&fx.main, "merge-sim", Some(&fx.host), &fx.temp);
        let mut first = ExactTreeVerificationSlot::acquire_at(&fx.main, plan()).unwrap();
        let expected = first.path.clone();
        assert!(!path_is_inside(&fx.main, &expected));
        std::fs::create_dir_all(&first.path).unwrap();
        std::fs::write(first.path.join("stale"), "old run").unwrap();
        first.cleanup();
        assert!(!expected.exists());
        drop(first);

        let second = ExactTreeVerificationSlot::acquire_at(&fx.main, plan()).unwrap();
        assert_eq!(second.path, expected);
    }
}
