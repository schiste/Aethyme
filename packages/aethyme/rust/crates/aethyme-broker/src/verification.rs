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
//! A slot follows the broker's already repository-scoped worktree root so a
//! checkout and the verification of its exact tree land on the same volume.
//! Host state remains the fallback for an unavailable configured root, and
//! system temporary storage is the fallback for unavailable host state. The
//! repository itself remains the last resort. Placement is planned without
//! creating paths; acquisition rechecks the selected volume before writing.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::file_lock::{ExclusiveFileLock, open_lock_file};
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
    /// Explicit environment/library roots are strict. If their storage is not
    /// safe, acquisition must fail rather than silently switching volumes.
    pub(crate) blocking_refusal: Option<String>,
}

/// Choose the directory this repository's slot will occupy.
pub(crate) fn plan_slot_placement(main_root: &Path, namespace: &str) -> SlotPlacement {
    let root_plan = default_worktree_root_plan(main_root);
    plan_slot_placement_with_worktree_root_plan(main_root, namespace, &root_plan)
}

/// Build the default worktree-root choice when no broker instance has an
/// accepted library override to supply.
pub(crate) fn default_worktree_root_plan(main_root: &Path) -> crate::WorktreeRootPlan {
    let repository = GitRepo::discover(main_root).ok();
    let git_common_dir = repository
        .as_ref()
        .and_then(|repository| repository.git_common_dir().ok());
    crate::broker::lifecycle::plan_worktree_root_for(
        main_root,
        git_common_dir.as_deref(),
        None,
    )
}

/// Place a verification slot using the exact root order accepted by its
/// broker. In particular, library and environment overrides remain strict.
pub(crate) fn plan_slot_placement_with_worktree_root_plan(
    main_root: &Path,
    namespace: &str,
    root_plan: &crate::WorktreeRootPlan,
) -> SlotPlacement {
    let key = root_plan.repository_key.clone();
    let candidates = crate::broker::lifecycle::worktree_root_candidates_for(root_plan);
    let strict = matches!(
        root_plan.preferred_source,
        Some(
            crate::WorktreeRootSource::EnvironmentOverride
                | crate::WorktreeRootSource::LibraryOverride
        )
    );
    plan_slot_placement_for_worktree_candidates(
        main_root,
        namespace,
        &candidates,
        &std::env::temp_dir(),
        &key,
        strict,
    )
}

/// [`plan_slot_placement`] with host and temporary roots supplied for tests.
#[cfg(test)]
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
    let host_state_root = host_state.map(|base| base.join(&key));
    let mut roots = Vec::new();
    if let Some(root) = host_state_root {
        roots.push((root, crate::WorktreeRootSource::HostState));
    }
    plan_slot_placement_for_worktree_candidates(
        main_root, namespace, &roots, temp_root, &key, false,
    )
}

#[cfg(test)]
fn plan_slot_placement_for_roots(
    main_root: &Path,
    namespace: &str,
    worktree_root: Option<&Path>,
    host_state_root: Option<&Path>,
    temp_root: &Path,
    repository_key: &str,
) -> SlotPlacement {
    let mut roots = Vec::new();
    if let Some(root) = worktree_root {
        roots.push((
            root.to_path_buf(),
            crate::WorktreeRootSource::RepositoryConfig,
        ));
    }
    if let Some(root) = host_state_root {
        roots.push((root.to_path_buf(), crate::WorktreeRootSource::HostState));
    }
    plan_slot_placement_for_worktree_candidates(
        main_root,
        namespace,
        &roots,
        temp_root,
        repository_key,
        false,
    )
}

fn plan_slot_placement_for_worktree_candidates(
    main_root: &Path,
    namespace: &str,
    worktree_roots: &[(PathBuf, crate::WorktreeRootSource)],
    temp_root: &Path,
    repository_key: &str,
    strict_override: bool,
) -> SlotPlacement {
    plan_slot_placement_for_worktree_candidates_with(
        main_root,
        namespace,
        worktree_roots,
        temp_root,
        repository_key,
        strict_override,
        removable_volume_unavailable_reason,
        |directory| slot_headroom_refusal(main_root, directory),
    )
}

#[allow(clippy::too_many_arguments)]
fn plan_slot_placement_for_worktree_candidates_with<VolumeRefusal, HeadroomRefusal>(
    main_root: &Path,
    namespace: &str,
    worktree_roots: &[(PathBuf, crate::WorktreeRootSource)],
    temp_root: &Path,
    repository_key: &str,
    strict_override: bool,
    mut volume_refusal: VolumeRefusal,
    mut headroom_refusal: HeadroomRefusal,
) -> SlotPlacement
where
    VolumeRefusal: FnMut(&Path) -> Option<String>,
    HeadroomRefusal: FnMut(&Path) -> Option<String>,
{
    let mut candidates = worktree_roots
        .iter()
        .map(|(root, _)| root.join("run").join(namespace))
        .collect::<Vec<_>>();
    if !strict_override {
        candidates.push(
            temp_root
                .join("aethyme-run")
                .join(repository_key)
                .join(namespace),
        );
    }
    candidates.dedup();

    let preferred = candidates.first().cloned();
    let mut refusals = Vec::new();
    let resolved_main_root = match crate::broker::lifecycle::canonical_prefix(main_root) {
        Ok(root) => root,
        Err(reason) => {
            let reason = format!(
                "cannot safely resolve repository root {}: {reason}",
                main_root.display()
            );
            return SlotPlacement {
                directory: main_root.to_path_buf(),
                inside_repository: true,
                preferred,
                refusals: vec![reason.clone()],
                blocking_refusal: Some(reason),
            };
        }
    };
    for candidate in candidates {
        let directory = match crate::broker::lifecycle::canonical_prefix(&candidate) {
            Ok(directory) => directory,
            Err(reason) => {
                let refusal = format!(
                    "{} cannot be safely resolved before slot creation: {reason}",
                    candidate.display()
                );
                refusals.push(refusal.clone());
                if strict_override {
                    return SlotPlacement {
                        directory: candidate,
                        inside_repository: true,
                        preferred,
                        refusals,
                        blocking_refusal: Some(refusal),
                    };
                }
                continue;
            }
        };
        if directory.starts_with(&resolved_main_root) {
            let reason = format!(
                "{} resolves inside repository {}",
                directory.display(),
                resolved_main_root.display()
            );
            refusals.push(reason.clone());
            if strict_override {
                return SlotPlacement {
                    directory,
                    inside_repository: true,
                    preferred,
                    refusals,
                    blocking_refusal: Some(reason),
                };
            }
            continue;
        }
        if let Some(reason) = volume_refusal(&directory) {
            let refusal = format!("{}: {reason}", directory.display());
            refusals.push(refusal.clone());
            if strict_override {
                return SlotPlacement {
                    directory,
                    inside_repository: false,
                    preferred,
                    refusals,
                    blocking_refusal: Some(refusal),
                };
            }
            continue;
        }
        if let Some(reason) = headroom_refusal(&directory) {
            let refusal = format!("{}: {reason}", directory.display());
            refusals.push(refusal.clone());
            if strict_override {
                return SlotPlacement {
                    directory,
                    inside_repository: false,
                    preferred,
                    refusals,
                    blocking_refusal: Some(refusal),
                };
            }
            continue;
        }
        return SlotPlacement {
            directory,
            inside_repository: false,
            preferred: None,
            refusals,
            blocking_refusal: None,
        };
    }
    repository_fallback_slot_placement(
        main_root,
        namespace,
        preferred,
        refusals,
        |directory| headroom_refusal(directory),
    )
}

fn repository_fallback_slot_placement(
    main_root: &Path,
    namespace: &str,
    preferred: Option<PathBuf>,
    mut refusals: Vec<String>,
    mut headroom_refusal: impl FnMut(&Path) -> Option<String>,
) -> SlotPlacement {
    let directory = main_root.join(".aethyme/run").join(namespace);
    let blocking_refusal = headroom_refusal(&directory).map(|reason| {
        let refusal = format!("{}: {reason}", directory.display());
        refusals.push(refusal.clone());
        refusal
    });
    SlotPlacement {
        directory,
        inside_repository: true,
        preferred,
        refusals,
        blocking_refusal,
    }
}

fn slot_headroom_refusal(main_root: &Path, directory: &Path) -> Option<String> {
    let nearest_directory = directory.ancestors().find(|ancestor| ancestor.exists())?;
    if !nearest_directory.is_dir() {
        return Some(format!(
            "{} is not a directory",
            nearest_directory.display()
        ));
    }
    let available = crate::disk_headroom::available_headroom_at_or_above_for(main_root, directory)?;
    crate::disk_headroom::refusal_with_headroom(
        Some(available.bytes),
        available.inodes,
        crate::DEFAULT_GATE_HEADROOM_BYTES,
        crate::disk_headroom::MIN_GATE_HEADROOM_INODES,
    )
}

pub(crate) fn removable_volume_unavailable_reason(path: &Path) -> Option<String> {
    let resolved = match crate::broker::lifecycle::canonical_prefix(path) {
        Ok(resolved) => resolved,
        Err(reason) => {
            return Some(format!(
                "cannot safely resolve {} before checking its volume: {reason}",
                path.display()
            ));
        }
    };
    removable_volume_unavailable_reason_with_root(&resolved, Path::new("/Volumes"))
}

fn removable_volume_unavailable_reason_with_root(path: &Path, volumes: &Path) -> Option<String> {
    let relative = path.strip_prefix(volumes).ok()?;
    let name = relative.components().next()?;
    let mount = volumes.join(name);
    if !mount.is_dir() {
        return Some(format!("volume {} is not mounted", mount.display()));
    }
    use std::os::unix::fs::MetadataExt;
    let (Ok(mount_metadata), Ok(volumes_metadata)) =
        (std::fs::metadata(&mount), std::fs::metadata(volumes))
    else {
        return Some(format!("cannot verify mounted volume {}", mount.display()));
    };
    (mount_metadata.dev() == volumes_metadata.dev()).then(|| {
        format!(
            "{} is not a mounted volume (it is a directory on the startup disk)",
            mount.display()
        )
    })
}

/// Conservatively treat paths with ambiguous missing-suffix traversal as
/// inside the repository. Placement itself reports the resolution refusal.
fn path_is_inside(root: &Path, candidate: &Path) -> bool {
    match (
        crate::broker::lifecycle::canonical_prefix(root),
        crate::broker::lifecycle::canonical_prefix(candidate),
    ) {
        (Ok(root), Ok(candidate)) => candidate.starts_with(root),
        _ => true,
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
    _lock: ExclusiveFileLock,
}

impl ExactTreeVerificationSlot {
    #[cfg(test)]
    pub(crate) fn acquire(main_root: &Path, namespace: &str) -> Result<Self, BrokerOpError> {
        let root_plan = default_worktree_root_plan(main_root);
        Self::acquire_with_worktree_root_plan(main_root, namespace, &root_plan)
    }

    pub(crate) fn acquire_with_worktree_root_plan(
        main_root: &Path,
        namespace: &str,
        root_plan: &crate::WorktreeRootPlan,
    ) -> Result<Self, BrokerOpError> {
        Self::acquire_at(
            main_root,
            plan_slot_placement_with_worktree_root_plan(main_root, namespace, root_plan),
        )
    }

    /// One of `size` stable slots, so up to `size` verifications run at once.
    ///
    /// A single repository-wide slot made merge verification strictly serial:
    /// measured at a flat ~8.6 submissions per hour whatever the number of
    /// submitting sessions, because gates ran sequentially inside one lock.
    /// Nothing about a slot requires two *different* verifications to share a
    /// directory -- each needs a stable, complete, non-nested checkout of its
    /// own tree.
    ///
    /// A fixed pool rather than one directory per commit, for two reasons.
    /// Each slot keeps a stable path, which is what lets build tools reuse
    /// path-sensitive fingerprints from one verification to the next; a fresh
    /// path per merge commit makes every verification start cold, and a merge
    /// commit is new on every submission even when the tree is not. And the
    /// set of directories stays bounded instead of growing by one lock file
    /// per verification forever.
    ///
    /// Slot 0 is the plain `namespace` placement, so the checkout every
    /// existing installation keeps warm stays the first one used. A free slot
    /// is taken without waiting; when all are busy this polls until one is
    /// released. Locks are `flock`s, so a holder that dies releases its slot.
    #[cfg(test)]
    pub(crate) fn acquire_pooled(
        main_root: &Path,
        namespace: &str,
        size: usize,
    ) -> Result<Self, BrokerOpError> {
        Self::acquire_pooled_reporting(main_root, namespace, size, Duration::MAX, &mut |_| {})
    }

    /// [`Self::acquire_pooled`], calling `on_wait` with the time waited as
    /// soon as every slot is found busy and then every `report_every` until
    /// one frees, so a caller can say it is queued instead of looking hung.
    #[cfg(test)]
    pub(crate) fn acquire_pooled_reporting(
        main_root: &Path,
        namespace: &str,
        size: usize,
        report_every: Duration,
        on_wait: &mut dyn FnMut(Duration),
    ) -> Result<Self, BrokerOpError> {
        let root_plan = default_worktree_root_plan(main_root);
        Self::acquire_pooled_reporting_with_worktree_root_plan(
            main_root,
            namespace,
            size,
            &root_plan,
            report_every,
            on_wait,
        )
    }

    pub(crate) fn acquire_pooled_reporting_with_worktree_root_plan(
        main_root: &Path,
        namespace: &str,
        size: usize,
        root_plan: &crate::WorktreeRootPlan,
        report_every: Duration,
        on_wait: &mut dyn FnMut(Duration),
    ) -> Result<Self, BrokerOpError> {
        let placements = (0..size.max(1))
            .map(|index| {
                let name = if index == 0 {
                    namespace.to_string()
                } else {
                    format!("{namespace}-{index}")
                };
                plan_slot_placement_with_worktree_root_plan(main_root, &name, root_plan)
            })
            .collect::<Vec<_>>();
        let started = std::time::Instant::now();
        let mut next_report = Duration::ZERO;
        loop {
            for placement in &placements {
                if let Some(slot) = Self::try_acquire_at(main_root, placement.clone())? {
                    return Ok(slot);
                }
            }
            let waited = started.elapsed();
            if waited >= next_report {
                on_wait(waited);
                next_report = waited.saturating_add(report_every);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Take the lock on an already chosen placement, or `None` if it is held.
    fn try_acquire_at(
        main_root: &Path,
        placement: SlotPlacement,
    ) -> Result<Option<Self>, BrokerOpError> {
        let lock_path = Self::prepare_lock_path(main_root, &placement)?;
        let file = open_lock_file(&lock_path).map_err(|source| BrokerError::Io {
            path: lock_path.clone(),
            source,
        })?;
        let lock = ExclusiveFileLock::try_acquire(file).map_err(|source| BrokerError::Io {
            path: lock_path,
            source,
        })?;
        Ok(lock.map(|lock| Self {
            repository_root: main_root.to_path_buf(),
            path: placement.directory.join("slot"),
            _lock: lock,
        }))
    }

    fn prepare_lock_path(
        main_root: &Path,
        placement: &SlotPlacement,
    ) -> Result<PathBuf, BrokerOpError> {
        if let Some(reason) = &placement.blocking_refusal {
            return Err(BrokerError::Io {
                path: placement.directory.clone(),
                source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, reason.clone()),
            }
            .into());
        }
        ensure_slot_storage_available(main_root, &placement.directory).map_err(|source| {
            BrokerError::Io {
                path: placement.directory.clone(),
                source,
            }
        })?;
        std::fs::create_dir_all(&placement.directory).map_err(|source| BrokerError::Io {
            path: placement.directory.clone(),
            source,
        })?;
        Ok(placement.directory.join("slot.lock"))
    }

    /// Take the lock on an already chosen placement.
    fn acquire_at(main_root: &Path, placement: SlotPlacement) -> Result<Self, BrokerOpError> {
        let lock_path = Self::prepare_lock_path(main_root, &placement)?;
        let file = open_lock_file(&lock_path).map_err(|source| BrokerError::Io {
            path: lock_path.clone(),
            source,
        })?;
        // Contention blocks rather than failing, so a lock error is a broken
        // lock and never a busy one. Answering it by moving to a different
        // directory would hand one slot to two processes, so it is fatal.
        let lock = ExclusiveFileLock::acquire(file).map_err(|source| BrokerError::Io {
            path: lock_path,
            source,
        })?;
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
        ensure_slot_storage_available(&self.repository_root, &self.path).map_err(|source| {
            BrokerError::Io {
                path: self.path.clone(),
                source,
            }
        })?;
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

fn ensure_slot_storage_available(main_root: &Path, path: &Path) -> Result<(), std::io::Error> {
    if let Some(reason) = removable_volume_unavailable_reason(path) {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, reason));
    }
    if let Some(reason) = slot_headroom_refusal(main_root, path) {
        return Err(std::io::Error::new(std::io::ErrorKind::StorageFull, reason));
    }
    Ok(())
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
        assert!(
            !placement.directory.exists(),
            "planning must not create {}",
            placement.directory.display()
        );
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
    fn a_missing_removable_mount_is_refused_without_creating_its_mountpoint() {
        let fx = fixture();
        let volumes = fx._root.path().join("volumes");
        std::fs::create_dir(&volumes).unwrap();
        let target = volumes.join("gone/worktrees/repo/run/merge-sim");

        let refusal = removable_volume_unavailable_reason_with_root(&target, &volumes)
            .expect("a missing mount must be refused");

        assert!(refusal.contains("is not mounted"), "{refusal}");
        assert!(!volumes.join("gone").exists());
    }

    #[test]
    fn a_worktree_root_slot_is_scoped_under_its_existing_repository_directory_once() {
        let fx = fixture();
        let worktree_root = fx._root.path().join("worktrees/repo-key");
        std::fs::create_dir_all(&worktree_root).unwrap();
        let placement = plan_slot_placement_for_roots(
            &fx.main,
            "merge-sim",
            Some(&worktree_root),
            None,
            &fx.temp,
            "repo-key",
        );

        assert_eq!(placement.directory, worktree_root.join("run/merge-sim"));
        assert!(
            !placement
                .directory
                .to_string_lossy()
                .contains("repo-key/repo-key"),
            "repository key was appended twice: {}",
            placement.directory.display()
        );
        assert!(!placement.directory.exists());
    }

    #[test]
    fn an_unavailable_explicit_worktree_root_does_not_fall_back_to_another_volume() {
        let fx = fixture();
        let unavailable_root = fx._root.path().join("not-a-directory");
        std::fs::write(&unavailable_root, "file blocks the root path").unwrap();
        let candidates = vec![(
            unavailable_root.clone(),
            crate::WorktreeRootSource::EnvironmentOverride,
        )];
        let placement = plan_slot_placement_for_worktree_candidates(
            &fx.main,
            "merge-sim",
            &candidates,
            &fx.temp,
            "repo-key",
            true,
        );
        let expected = unavailable_root.join("run/merge-sim");

        assert!(placement.blocking_refusal.is_some());
        assert_eq!(placement.preferred.as_deref(), Some(expected.as_path()));
        assert!(!expected.exists());
        assert!(
            !fx.temp.join("aethyme-run/repo-key/merge-sim").exists(),
            "an explicit root must not fall back to a different volume"
        );
        assert!(
            ExactTreeVerificationSlot::acquire_at(&fx.main, placement).is_err(),
            "an unavailable explicit root must fail before creating a slot"
        );
        assert!(!expected.exists());
    }

    #[test]
    fn a_library_worktree_root_places_slots_under_the_accepted_root() {
        let fx = fixture();
        let worktree_root = fx._root.path().join("library-worktrees");
        std::fs::create_dir_all(&worktree_root).unwrap();
        let root_plan = crate::broker::lifecycle::plan_worktree_root_for(
            &fx.main,
            None,
            Some(&worktree_root),
        );

        let placement =
            plan_slot_placement_with_worktree_root_plan(&fx.main, "merge-sim", &root_plan);

        assert_eq!(placement.directory, worktree_root.join("run/merge-sim"));
        assert_eq!(placement.preferred, None);
        assert!(!placement.inside_repository);
        assert!(!placement.directory.exists());
    }

    #[test]
    fn an_unavailable_library_worktree_root_is_strict_and_creates_no_fallback() {
        let fx = fixture();
        let unavailable_root = fx._root.path().join("not-a-directory");
        std::fs::write(&unavailable_root, "file blocks the root path").unwrap();
        let root_plan = crate::broker::lifecycle::plan_worktree_root_for(
            &fx.main,
            None,
            Some(&unavailable_root),
        );
        let expected = unavailable_root.join("run/merge-sim");

        let placement =
            plan_slot_placement_with_worktree_root_plan(&fx.main, "merge-sim", &root_plan);

        assert!(placement.blocking_refusal.is_some());
        assert_eq!(placement.preferred.as_deref(), Some(expected.as_path()));
        assert!(!expected.exists());
        assert!(
            ExactTreeVerificationSlot::acquire_with_worktree_root_plan(
                &fx.main,
                "merge-sim",
                &root_plan,
            )
            .is_err(),
            "an unavailable library override must fail before creating a slot"
        );
        assert!(!expected.exists());
        assert!(
            !fx.temp.join("aethyme-run/repo-key/merge-sim").exists(),
            "a library override must not fall back to temporary storage"
        );
    }

    #[test]
    fn a_strict_override_with_unresolved_parent_traversal_creates_no_slot_artifacts() {
        let fx = fixture();
        std::fs::create_dir_all(&fx.main).unwrap();
        let missing = fx._root.path().join("missing");
        // The unresolved traversal would end under the checkout if followed.
        let override_root = fx._root.path().join("missing/../repo/.aethyme/worktrees");
        let root_plan = crate::broker::lifecycle::plan_worktree_root_for(
            &fx.main,
            None,
            Some(&override_root),
        );

        let placement =
            plan_slot_placement_with_worktree_root_plan(&fx.main, "merge-sim", &root_plan);

        assert!(placement.blocking_refusal.is_some());
        assert!(
            placement
                .blocking_refusal
                .as_deref()
                .is_some_and(|reason| reason.contains("ambiguous path resolution")),
            "{:?}",
            placement.blocking_refusal
        );
        assert!(
            ExactTreeVerificationSlot::acquire_with_worktree_root_plan(
                &fx.main,
                "merge-sim",
                &root_plan,
            )
            .is_err()
        );
        assert!(!missing.exists());
        assert!(!fx.main.join(".aethyme").exists());
        assert!(!fx.temp.join("aethyme-run").exists());
    }

    #[test]
    fn a_strict_override_with_low_bytes_or_inodes_does_not_create_a_fallback() {
        let fx = fixture();
        let override_root = fx._root.path().join("library-worktrees");
        let candidates = vec![(
            override_root.clone(),
            crate::WorktreeRootSource::LibraryOverride,
        )];

        for reason in [
            "available bytes are below the gate reserve",
            "available inodes are below the gate reserve",
        ] {
            let placement = plan_slot_placement_for_worktree_candidates_with(
                &fx.main,
                "merge-sim",
                &candidates,
                &fx.temp,
                "repo-key",
                true,
                |_| None,
                |_| Some(reason.to_string()),
            );
            let expected = override_root.join("run/merge-sim");

            assert!(placement.blocking_refusal.is_some(), "{reason}");
            assert!(placement.refusals[0].contains(reason));
            assert_eq!(placement.directory, expected);
            assert!(!expected.exists());
            assert!(ExactTreeVerificationSlot::acquire_at(&fx.main, placement).is_err());
            assert!(!expected.exists());
            assert!(
                !fx.temp.join("aethyme-run/repo-key/merge-sim").exists(),
                "a strict override must not fall back after {reason}"
            );
        }
    }

    #[test]
    fn the_repository_fallback_refuses_low_headroom_without_creating_a_slot() {
        let fx = fixture();
        let preferred = fx._root.path().join("external/repo-key/run/merge-sim");
        let placement = repository_fallback_slot_placement(
            &fx.main,
            "merge-sim",
            Some(preferred),
            Vec::new(),
            |_| Some("available headroom is below the gate reserve".into()),
        );
        let expected = fx.main.join(".aethyme/run/merge-sim");

        assert!(placement.inside_repository);
        assert!(placement.blocking_refusal.is_some());
        assert_eq!(placement.directory, expected);
        assert!(!expected.exists());
        assert!(ExactTreeVerificationSlot::acquire_at(&fx.main, placement).is_err());
        assert!(!expected.exists());
    }

    #[test]
    fn two_namespaces_never_share_a_directory() {
        let fx = fixture();
        assert_ne!(
            plan_slot_placement_in(&fx.main, "merge-sim", Some(&fx.host), &fx.temp).directory,
            plan_slot_placement_in(&fx.main, "graph-integrity", Some(&fx.host), &fx.temp).directory
        );
    }

    /// A verification that finds slot 0 busy takes another slot instead of
    /// waiting: this is the whole throughput change. Run on a thread with a
    /// timeout so a regression to "block on one slot" fails rather than hangs.
    #[test]
    fn a_busy_slot_does_not_block_a_second_verification() {
        let fx = fixture();
        let first = ExactTreeVerificationSlot::acquire_pooled(&fx.main, "merge-sim", 2).unwrap();
        let main = fx.main.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let second = ExactTreeVerificationSlot::acquire_pooled(&main, "merge-sim", 2).unwrap();
            sender.send(second.path.clone()).unwrap();
        });
        let second = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("a second verification waited for a busy slot while another was free");
        assert_ne!(first.path, second, "two verifications shared one checkout");
    }

    /// Paths are stable and bounded: releasing and re-acquiring lands on the
    /// same checkout, and slot 0 is the existing single-slot placement, so the
    /// directory installations already keep warm stays the one used first.
    #[test]
    fn pooled_slots_keep_stable_paths_and_slot_zero_is_the_existing_placement() {
        let fx = fixture();
        let first = ExactTreeVerificationSlot::acquire_pooled(&fx.main, "merge-sim", 4).unwrap();
        let path = first.path.clone();
        drop(first);
        let again = ExactTreeVerificationSlot::acquire_pooled(&fx.main, "merge-sim", 4).unwrap();
        assert_eq!(again.path, path, "a released slot was not reused");
        drop(again);
        let single = ExactTreeVerificationSlot::acquire(&fx.main, "merge-sim").unwrap();
        assert_eq!(
            single.path, path,
            "slot 0 moved away from the existing placement"
        );
    }

    /// With every slot busy a verification waits, and takes the slot as soon as
    /// it is released -- the pool bounds concurrency, it never fails a submit.
    #[test]
    fn a_full_pool_waits_for_a_released_slot() {
        let fx = fixture();
        let held = ExactTreeVerificationSlot::acquire_pooled(&fx.main, "merge-sim", 1).unwrap();
        let held_path = held.path.clone();
        let main = fx.main.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let slot = ExactTreeVerificationSlot::acquire_pooled(&main, "merge-sim", 1).unwrap();
            sender.send(slot.path.clone()).unwrap();
        });
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(600))
                .is_err(),
            "a verification entered a slot another one still holds"
        );
        drop(held);
        let path = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("a released slot was never taken");
        assert_eq!(path, held_path);
    }

    /// A free slot is taken without a word; a full pool says so at once and
    /// again on every report interval, so a queued submit never looks hung.
    #[test]
    fn a_waiting_verification_reports_at_once_and_then_on_every_interval() {
        let fx = fixture();
        let free_waits = std::cell::Cell::new(0);
        drop(
            ExactTreeVerificationSlot::acquire_pooled_reporting(
                &fx.main,
                "merge-sim",
                1,
                Duration::from_millis(100),
                &mut |_| free_waits.set(free_waits.get() + 1),
            )
            .unwrap(),
        );
        assert_eq!(free_waits.get(), 0, "a free slot reported a wait");

        let held = ExactTreeVerificationSlot::acquire_pooled(&fx.main, "merge-sim", 1).unwrap();
        let main = fx.main.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            ExactTreeVerificationSlot::acquire_pooled_reporting(
                &main,
                "merge-sim",
                1,
                Duration::from_millis(100),
                &mut |waited| sender.send(waited).unwrap(),
            )
            .unwrap()
        });
        let first = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("a blocked verification said nothing");
        assert!(
            first < Duration::from_millis(100),
            "the first report waited {first:?}"
        );
        let later = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("no report on the next interval");
        assert!(later >= Duration::from_millis(100), "{later:?}");
        drop(held);
        waiter.join().unwrap();
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
