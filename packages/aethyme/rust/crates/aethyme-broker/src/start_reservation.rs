//! Narrow per-checkout claims that let `broker start` run `git worktree add`
//! outside the repository-wide lifecycle lock.
//!
//! `start` used to hold the lifecycle lock from slug selection through
//! `git worktree add` and session registration, so every other start, finish
//! and cleanup in the repository waited for the slowest checkout. Now the
//! wide lock covers only the cheap, repository-wide decisions: reaping dead
//! reservations, choosing a unique slug, and recording this start's
//! reservation. The slow work then runs under the reservation alone.
//!
//! A reservation is one file, `.aethyme/run/start-reservations/<slug>.lock`,
//! that its start holds an exclusive `flock` on, plus the destination
//! directory, created empty while the wide lock is held.
//!
//! Invariants:
//!
//! 1. A reservation file and its destination directory are created only
//!    under the wide lifecycle lock, file first. So any destination a
//!    reservation created is covered by that file until the file is removed.
//! 2. Liveness is the `flock` itself: the kernel releases it when the owning
//!    process exits, however it exits. A reservation whose lock can be taken
//!    belongs to a dead start. There is no heartbeat or reap interval: the next
//!    start (under the wide lock) reaps it.
//! 3. Reaping runs only under the wide lock, so it never races a new
//!    reservation. It removes the destination only while that destination is
//!    an empty directory, and the branch only while the destination holds no
//!    `.git` and the branch still names the recorded base. Anything more
//!    (a partial or complete checkout) is left to the existing recovery
//!    lanes, exactly as when a start died under the wide lock.
//! 4. A start releases its reservation only after its session row is
//!    committed (row-based protection then applies) or after its own rollback
//!    finished. Release unlinks the file before unlocking it, and every remover
//!    first checks that the path still names the inode it locked, so a release
//!    racing a reaper can neither unlink a newer reservation nor delete
//!    anything the reaper would not delete for a dead start.
//! 5. Slug selection treats a reserved slug as taken even before its
//!    destination or branch exists, so two starts never pick the same path or
//!    branch.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

const START_RESERVATIONS_DIR: &str = "start-reservations";
const RESERVATION_SUFFIX: &str = ".lock";
const MAX_RECORD_BYTES: u64 = 8192;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ReservationRecord {
    pub(crate) pid: u32,
    /// The owner's process start time, for diagnostics only; liveness is the
    /// lock (invariant 2).
    #[serde(default)]
    pub(crate) process_start: u64,
    pub(crate) worktree_path: PathBuf,
    pub(crate) branch: String,
    pub(crate) base: String,
    pub(crate) created_at_ms: i64,
}

/// The reason every removal lane records when it refuses a reserved path.
pub(crate) const START_IN_PROGRESS: &str = "start_in_progress";

/// Destinations this process holds reservations for. Its own rollback may
/// remove them; every other remover refuses (see [`refuse_if_start_in_progress`]).
fn held_here() -> &'static std::sync::Mutex<std::collections::BTreeSet<PathBuf>> {
    static HELD: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<PathBuf>>> =
        std::sync::OnceLock::new();
    HELD.get_or_init(Default::default)
}

fn comparable(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        path.parent()
            .and_then(|parent| parent.canonicalize().ok())
            .zip(path.file_name())
            .map_or_else(|| path.to_path_buf(), |(parent, name)| parent.join(name))
    })
}

/// A live reservation held by this process. Dropping it releases it.
#[derive(Debug)]
pub(crate) struct StartReservation {
    file: Option<File>,
    lock_path: PathBuf,
    worktree_path: Option<PathBuf>,
}

impl StartReservation {
    /// Release now: unlink the file while still holding the lock, then
    /// unlock (invariant 4).
    pub(crate) fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        let Some(file) = self.file.take() else {
            return;
        };
        if let Some(path) = self.worktree_path.take()
            && let Ok(mut held) = held_here().lock()
        {
            held.remove(&path);
        }
        if same_inode(&self.lock_path, &file) {
            crate::warn_unrecorded(
                "remove the start reservation file",
                std::fs::remove_file(&self.lock_path),
            );
        }
        // Best effort: closing the file releases the lock anyway.
        file.unlock().ok();
    }
}

impl Drop for StartReservation {
    fn drop(&mut self) {
        self.release_inner();
    }
}

fn reservations_directory(main_root: &Path) -> io::Result<PathBuf> {
    let directory =
        crate::file_lock::repository_run_directory(main_root)?.join(START_RESERVATIONS_DIR);
    match std::fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(directory),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "start reservation path is not a real directory: {}",
                directory.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match std::fs::create_dir(&directory) {
                Ok(()) => Ok(directory),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(directory),
                Err(error) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

/// The reservations directory only if it already exists; readers never create
/// it.
fn existing_reservations_directory(main_root: &Path) -> Option<PathBuf> {
    let directory = main_root
        .canonicalize()
        .ok()?
        .join(".aethyme/run")
        .join(START_RESERVATIONS_DIR);
    std::fs::symlink_metadata(&directory)
        .ok()
        .filter(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        .map(|_| directory)
}

fn reservation_path(directory: &Path, slug: &str) -> PathBuf {
    directory.join(format!("{slug}{RESERVATION_SUFFIX}"))
}

fn open_existing(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn same_inode(path: &Path, file: &File) -> bool {
    let (Ok(named), Ok(opened)) = (std::fs::symlink_metadata(path), file.metadata()) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        named.dev() == opened.dev() && named.ino() == opened.ino()
    }
    #[cfg(not(unix))]
    {
        named.is_file() && opened.is_file()
    }
}

fn read_record(file: &File) -> Option<ReservationRecord> {
    use std::io::{Read as _, Seek as _};
    let mut bytes = Vec::new();
    let mut reader = file.try_clone().ok()?;
    reader.seek(io::SeekFrom::Start(0)).ok()?;
    reader.take(MAX_RECORD_BYTES).read_to_end(&mut bytes).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Whether `slug` has a reservation file, live or not. Called under the wide
/// lock after [`reap_stale`], so a remaining file is live or unreapable; both
/// keep the slug taken (invariant 5).
pub(crate) fn slug_is_reserved(main_root: &Path, slug: &str) -> bool {
    existing_reservations_directory(main_root)
        .is_some_and(|directory| reservation_path(&directory, slug).exists())
}

/// Outcome of creating a reservation.
#[derive(Debug)]
pub(crate) enum Reserve {
    Reserved(StartReservation),
    /// Someone else holds the slug or its destination already exists.
    Taken,
}

/// Reserve `slug` for `worktree_path` and `branch`. The caller must hold the
/// wide lifecycle lock (invariant 1). Creates the reservation file, locks it,
/// writes the record, then creates the destination directory empty.
pub(crate) fn reserve(
    main_root: &Path,
    slug: &str,
    worktree_path: &Path,
    branch: &str,
    base: &str,
) -> io::Result<Reserve> {
    let directory = reservations_directory(main_root)?;
    let lock_path = reservation_path(&directory, slug);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
        options.mode(0o600);
    }
    let file = match options.open(&lock_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(Reserve::Taken),
        Err(error) => return Err(error),
    };
    // Only a reader probing liveness can hold this brand-new file, and only
    // for an instant (reapers and other reservations need the wide lock we
    // hold), so wait for it rather than abandon the slug.
    if let Err(error) = file.lock() {
        crate::warn_unrecorded(
            "remove an unlockable start reservation file",
            std::fs::remove_file(&lock_path),
        );
        return Err(error);
    }
    // From here on, dropping `reservation` unlinks and unlocks the file.
    let mut reservation = StartReservation {
        file: Some(file),
        lock_path,
        worktree_path: None,
    };
    let record = ReservationRecord {
        pid: std::process::id(),
        process_start: i32::try_from(std::process::id())
            .ok()
            .and_then(crate::gates::process_start_time)
            .unwrap_or(0),
        worktree_path: worktree_path.to_path_buf(),
        branch: branch.to_string(),
        base: base.to_string(),
        created_at_ms: crate::clock::epoch_ms(),
    };
    let bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
    reservation
        .file
        .as_mut()
        .expect("a new reservation holds its file")
        .write_all(&bytes)?;
    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::create_dir(worktree_path) {
        Ok(()) => {
            let held = comparable(worktree_path);
            if let Ok(mut set) = held_here().lock() {
                set.insert(held.clone());
            }
            reservation.worktree_path = Some(held);
            Ok(Reserve::Reserved(reservation))
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            drop(reservation);
            Ok(Reserve::Taken)
        }
        Err(error) => Err(error),
    }
}

/// What reaping dead reservations did.
#[derive(Debug, Default)]
pub(crate) struct ReapReport {
    pub(crate) reaped: usize,
    pub(crate) removed_directories: usize,
    pub(crate) removed_branches: usize,
    /// Reservations kept because their leftovers need the recovery lanes.
    pub(crate) retained: Vec<String>,
}

/// Reap reservations whose owner died. The caller must hold the wide
/// lifecycle lock (invariant 3). `delete_branch_at` deletes `branch` only if
/// it still names `base`; it is the caller's checked deletion.
pub(crate) fn reap_stale(
    main_root: &Path,
    branch_tip: impl Fn(&str) -> Option<String>,
    delete_branch_at: impl Fn(&str, &str) -> Result<(), String>,
) -> ReapReport {
    let mut report = ReapReport::default();
    let Some(directory) = existing_reservations_directory(main_root) else {
        return report;
    };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return report;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| !name.ends_with(RESERVATION_SUFFIX))
        {
            continue;
        }
        let Ok(file) = open_existing(&path) else {
            continue;
        };
        match file.try_lock() {
            Ok(()) => {}
            // Live: its start is still running (invariant 2).
            Err(_) => continue,
        }
        if !same_inode(&path, &file) {
            // Released (and unlinked) while we opened it.
            file.unlock().ok();
            continue;
        }
        if let Some(record) = read_record(&file) {
            match reap_leftovers(&record, &branch_tip, &delete_branch_at) {
                Ok((directory_removed, branch_removed)) => {
                    report.removed_directories += usize::from(directory_removed);
                    report.removed_branches += usize::from(branch_removed);
                }
                Err(reason) => report.retained.push(reason),
            }
        }
        // A record that cannot be read was written by a start that died
        // before its destination existed (invariant 1: file first).
        crate::warn_unrecorded(
            "remove a dead start reservation file",
            std::fs::remove_file(&path),
        );
        file.unlock().ok();
        report.reaped += 1;
    }
    report
}

fn reap_leftovers(
    record: &ReservationRecord,
    branch_tip: &impl Fn(&str) -> Option<String>,
    delete_branch_at: &impl Fn(&str, &str) -> Result<(), String>,
) -> Result<(bool, bool), String> {
    let path = &record.worktree_path;
    let directory_removed = match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            // `remove_dir` refuses a nonempty directory, so a partial or
            // complete checkout is never removed here.
            match std::fs::remove_dir(path) {
                Ok(()) => true,
                Err(_) => {
                    return Err(format!(
                        "dead start left {} with contents (branch {}); the existing recovery lanes own it",
                        path.display(),
                        record.branch
                    ));
                }
            }
        }
        _ => {
            return Err(format!(
                "dead start's destination {} is not a directory; left in place",
                path.display()
            ));
        }
    };
    if path.join(".git").exists() {
        return Ok((directory_removed, false));
    }
    let branch_removed = match branch_tip(&record.branch) {
        Some(tip) if tip == record.base => {
            delete_branch_at(&record.branch, &record.base).map_err(|error| {
                format!(
                    "dead start left branch {} at its base; deleting it failed: {error}",
                    record.branch
                )
            })?;
            true
        }
        Some(_) => {
            return Err(format!(
                "dead start's branch {} moved past its base; left in place",
                record.branch
            ));
        }
        None => false,
    };
    Ok((directory_removed, branch_removed))
}

/// Live reservations, by record. A reader lock taken on a dead reservation is
/// released at once; it never removes anything.
fn live_records(main_root: &Path) -> Vec<ReservationRecord> {
    let Some(directory) = existing_reservations_directory(main_root) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return Vec::new();
    };
    let mut live = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| !name.ends_with(RESERVATION_SUFFIX))
        {
            continue;
        }
        let Ok(file) = open_existing(&path) else {
            continue;
        };
        let record = read_record(&file);
        match file.try_lock() {
            Ok(()) => {
                // A dead reservation: release the probe at once.
                file.unlock().ok();
            }
            Err(_) => live.extend(record),
        }
    }
    live
}

/// Whether a running `broker start` has reserved checkout `path` of the
/// repository whose primary checkout is `main_root`: its reservation file
/// exists and its lock is held (by any process, this one included). This is
/// the stable helper every lane that retires, trashes, cleans, strips or
/// removes a checkout consults under the lifecycle claim.
pub(crate) fn path_is_reserved(main_root: &Path, path: &Path) -> bool {
    let wanted = comparable(path);
    live_records(main_root)
        .iter()
        .any(|record| record.worktree_path == path || comparable(&record.worktree_path) == wanted)
}

/// Refuse removing `path` while another start has it reserved; `Err` carries
/// the recorded reason, starting with [`START_IN_PROGRESS`]. A reservation this
/// process holds is its own start's rollback and is allowed.
pub(crate) fn refuse_if_start_in_progress(main_root: &Path, path: &Path) -> Result<(), String> {
    if held_here()
        .lock()
        .is_ok_and(|held| held.contains(&comparable(path)))
    {
        return Ok(());
    }
    if path_is_reserved(main_root, path) {
        return Err(format!(
            "{START_IN_PROGRESS}: a running broker start has reserved {}; it is not removed",
            path.display()
        ));
    }
    Ok(())
}

/// Checkout paths of live reservations. Each one is a checkout that has no
/// session row yet (a start releases its reservation once its row is
/// committed), so the checkout limit and the free-space reserve count them.
pub(crate) fn live_reserved_paths(main_root: &Path) -> Vec<PathBuf> {
    live_records(main_root)
        .into_iter()
        .map(|record| record.worktree_path)
        .collect()
}

/// Test seam: runs once, on this thread, after `start` released the
/// lifecycle claim and before `git worktree add`, with the reserved
/// destination and branch.
#[cfg(test)]
pub(crate) type BeforeAddHook = Box<dyn FnOnce(&Path, &str)>;

#[cfg(test)]
thread_local! {
    static BEFORE_ADD_HOOK: std::cell::RefCell<Option<BeforeAddHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_before_add_hook(hook: BeforeAddHook) {
    BEFORE_ADD_HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
}

/// Run (and clear) the test seam, if one is set on this thread.
pub(crate) fn run_before_add_hook(_destination: &Path, _branch: &str) {
    #[cfg(test)]
    {
        let hook = BEFORE_ADD_HOOK.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            hook(_destination, _branch);
        }
    }
}

#[cfg(test)]
thread_local! {
    static RECORDED_PATH_REUSE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Test seam for fixtures that need a successor checkout at a predecessor's
/// exact path (as adoption or reuse produces): while the guard lives, `start`
/// on this thread may hand out a path an old session row records.
#[cfg(test)]
pub(crate) struct AllowRecordedPathReuse(());

#[cfg(test)]
impl AllowRecordedPathReuse {
    pub(crate) fn enter() -> Self {
        RECORDED_PATH_REUSE.with(|flag| flag.set(true));
        Self(())
    }
}

#[cfg(test)]
impl Drop for AllowRecordedPathReuse {
    fn drop(&mut self) {
        RECORDED_PATH_REUSE.with(|flag| flag.set(false));
    }
}

/// Whether the test seam above is active on this thread; always false in
/// production builds.
pub(crate) fn recorded_path_reuse_allowed() -> bool {
    #[cfg(test)]
    {
        RECORDED_PATH_REUSE.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// What a killed start leaves: a reservation file whose lock nobody holds.
#[cfg(test)]
pub(crate) fn write_dead_reservation(
    main_root: &Path,
    slug: &str,
    destination: &Path,
    branch: &str,
    base: &str,
) {
    let directory = reservations_directory(main_root).unwrap();
    let record = ReservationRecord {
        pid: 0,
        process_start: 0,
        worktree_path: destination.to_path_buf(),
        branch: branch.to_string(),
        base: base.into(),
        created_at_ms: 0,
    };
    std::fs::write(
        reservation_path(&directory, slug),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
}

/// What a start running in another process holds: a reservation whose lock
/// is held, but not one this process's own rollback may remove. Dropping the
/// returned file releases it.
#[cfg(test)]
pub(crate) fn hold_foreign_reservation(
    main_root: &Path,
    slug: &str,
    destination: &Path,
    branch: &str,
    base: &str,
) -> File {
    write_dead_reservation(main_root, slug, destination, branch, base);
    let directory = reservations_directory(main_root).unwrap();
    let file = open_existing(&reservation_path(&directory, slug)).unwrap();
    file.lock().unwrap();
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    fn main_root() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".aethyme/run")).unwrap();
        tmp
    }

    #[test]
    fn a_reservation_takes_the_slug_and_creates_an_empty_destination() {
        let root = main_root();
        let destination = root.path().join("pool/task");
        let Reserve::Reserved(reservation) =
            reserve(root.path(), "task", &destination, "agent/task", "base").unwrap()
        else {
            panic!("first reservation must succeed");
        };
        assert!(destination.is_dir());
        assert!(slug_is_reserved(root.path(), "task"));
        assert!(path_is_reserved(root.path(), &destination));
        assert_eq!(live_reserved_paths(root.path()), vec![destination.clone()]);
        assert!(matches!(
            reserve(root.path(), "task", &destination, "agent/task", "base").unwrap(),
            Reserve::Taken
        ));
        // This process's own reservation is its rollback's to remove.
        assert!(refuse_if_start_in_progress(root.path(), &destination).is_ok());
        reservation.release();
        assert!(!slug_is_reserved(root.path(), "task"));
        assert!(!path_is_reserved(root.path(), &destination));
    }

    #[test]
    fn another_start_s_reservation_is_refused_with_start_in_progress() {
        let root = main_root();
        let destination = root.path().join("pool/task");
        std::fs::create_dir_all(&destination).unwrap();
        let held = hold_foreign_reservation(root.path(), "task", &destination, "agent/task", "b");
        let reason = refuse_if_start_in_progress(root.path(), &destination).unwrap_err();
        assert!(reason.starts_with(START_IN_PROGRESS), "{reason}");
        drop(held);
        assert!(refuse_if_start_in_progress(root.path(), &destination).is_ok());
    }

    #[test]
    fn an_existing_destination_is_never_reserved() {
        let root = main_root();
        let destination = root.path().join("pool/task");
        std::fs::create_dir_all(&destination).unwrap();
        assert!(matches!(
            reserve(root.path(), "task", &destination, "agent/task", "base").unwrap(),
            Reserve::Taken
        ));
        assert!(!slug_is_reserved(root.path(), "task"));
    }

    #[test]
    fn a_live_reservation_is_never_reaped() {
        let root = main_root();
        let destination = root.path().join("pool/task");
        let Reserve::Reserved(_reservation) =
            reserve(root.path(), "task", &destination, "agent/task", "base").unwrap()
        else {
            panic!("reservation must succeed");
        };
        let report = reap_stale(
            root.path(),
            |_| panic!("a live reservation's branch is never inspected"),
            |_, _| panic!("a live reservation's branch is never deleted"),
        );
        assert_eq!(report.reaped, 0);
        assert!(destination.is_dir());
        assert!(slug_is_reserved(root.path(), "task"));
    }

    fn dead_reservation(root: &Path, slug: &str, destination: &Path, base: &str) {
        write_dead_reservation(root, slug, destination, &format!("agent/{slug}"), base);
    }

    #[test]
    fn a_dead_reservation_is_reaped_and_its_empty_leftovers_removed() {
        let root = main_root();
        let destination = root.path().join("pool/task");
        std::fs::create_dir_all(&destination).unwrap();
        dead_reservation(root.path(), "task", &destination, "base");
        let deleted = std::cell::RefCell::new(Vec::new());
        let report = reap_stale(
            root.path(),
            |branch| (branch == "agent/task").then(|| "base".to_string()),
            |branch, base| {
                deleted
                    .borrow_mut()
                    .push((branch.to_string(), base.to_string()));
                Ok(())
            },
        );
        assert_eq!(report.reaped, 1);
        assert_eq!(report.removed_directories, 1);
        assert_eq!(report.removed_branches, 1);
        assert!(!destination.exists());
        assert_eq!(
            deleted.into_inner(),
            vec![("agent/task".to_string(), "base".to_string())]
        );
        assert!(!slug_is_reserved(root.path(), "task"));
        // The name is reusable at once.
        assert!(matches!(
            reserve(root.path(), "task", &destination, "agent/task", "base").unwrap(),
            Reserve::Reserved(_)
        ));
    }

    #[test]
    fn a_dead_reservation_never_removes_a_checkout_or_a_moved_branch() {
        let root = main_root();
        let destination = root.path().join("pool/task");
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(destination.join(".git"), "gitdir: elsewhere\n").unwrap();
        dead_reservation(root.path(), "task", &destination, "base");
        let report = reap_stale(
            root.path(),
            |_| Some("base".to_string()),
            |_, _| panic!("a branch checked out by a leftover checkout is never deleted"),
        );
        assert_eq!(report.reaped, 1);
        assert_eq!(report.removed_directories, 0);
        assert_eq!(report.retained.len(), 1);
        assert!(destination.join(".git").is_file());

        let other = root.path().join("pool/other");
        dead_reservation(root.path(), "other", &other, "base");
        let report = reap_stale(
            root.path(),
            |_| Some("moved".to_string()),
            |_, _| panic!("a branch past its base is never deleted"),
        );
        assert_eq!(report.reaped, 1);
        assert_eq!(report.removed_branches, 0);
        assert_eq!(report.retained.len(), 1);
    }
}
