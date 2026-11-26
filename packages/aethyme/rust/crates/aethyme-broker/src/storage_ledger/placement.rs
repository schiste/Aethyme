//! Where recovery archives and gate caches are written: in host state and the
//! host cache (the default), or on an adopted storage root on another volume.
//!
//! The owner chooses in `<host state>/storage.toml`, beside `[budget]`:
//!
//! ```toml
//! [placement]
//! recovery_archives = "storage"   # "host" (default) or "storage"
//! gate_caches = "storage"         # "host" (default) or "storage"
//! storage_root = "/path/to/root"  # optional: needed only when several
//!                                 # roots were adopted
//! ```
//!
//! Only an adopted root qualifies ([`qualify`]): a real directory (not a
//! symbolic link) carrying the [`STORAGE_ROOT_MARKER`], both owned by this
//! user, on a local file system, on a volume that is mounted. When it does
//! not, everything is written in host state and the host cache as before and
//! the reason is counted (`storage_placement_fallback_<reason>`); a missing
//! volume never fails a cleanup.
//!
//! On a storage root, the layout mirrors host state and the host cache:
//! `<root>/recovery-archives/<repository key>/<archive>` and
//! `<root>/gates/<repository key>/<cache key>`, so the storage worker
//! inventories, expires, floors and lists them exactly as it does in host
//! state, and an archive keeps its id (`<repository key>/<archive>`).
//!
//! Readers find archives in every location whatever the setting: the
//! broker's archive roots include `<root>/recovery-archives/<key>` for every
//! storage root that ever received archives ([`ARCHIVE_STORAGE_ROOTS_FILE`],
//! written before the first archive lands there) and every adopted one.
//!
//! Migration ([`migrate_archives`]): while recovery archives are placed on a
//! storage root, each storage worker pass moves archives already in host
//! state there, oldest name first from a persisted cursor, within
//! [`MIGRATION_BUDGET`]. Never a partial archive (being written), never one
//! without a manifest, and never while its repository's lifecycle lock is
//! held or awaited (a restore or retirement in progress). Same volume: one
//! rename. Another volume: copied into a `.partial` staging directory beside
//! the destination, every file and directory synced, both trees compared
//! entry by entry (type, size, SHA-256), renamed into place, and only then is
//! the source renamed aside and deleted. A crash leaves either the source
//! intact (the staging is removed on the next attempt) or two identical
//! copies (the next attempt proves them identical and removes the source).
//! Keep and dismissal markers are copied before the archive and removed from
//! the source after it.
//!
//! Gate caches are a cache: a host gate cache is never moved or reused once
//! the placement is on a storage root; the worker evicts it
//! ([`evict_superseded_host_gate_caches`]) under its exclusive lease, so a
//! gate still running on it keeps it.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::evict::{self, Candidate, Reason};
use super::{BUDGET_FILE, Class, Item, Ledger, RootKind, STORAGE_ROOT_MARKER, write_atomically};

/// Recovery archives' directory in host state and on a storage root.
pub const RECOVERY_ARCHIVE_DIRECTORY: &str = "recovery-archives";
/// Gate caches' directory in the host cache and on a storage root.
pub const GATE_CACHE_DIRECTORY: &str = "gates";
/// Storage roots that received recovery archives
/// (`<host state>/storage-archive-roots.json`): while one is unreachable, a
/// decision that deletes when it finds no archive cannot be made.
pub const ARCHIVE_STORAGE_ROOTS_FILE: &str = "storage-archive-roots.json";
/// The lock serializing writers of [`ARCHIVE_STORAGE_ROOTS_FILE`].
pub const ARCHIVE_STORAGE_ROOTS_LOCK_FILE: &str = "storage-archive-roots.lock";
/// The migration's cursor and counters (`<host state>/storage-placement.json`).
pub const MIGRATION_STATE_FILE: &str = "storage-placement.json";
/// The longest one pass spends moving archives.
pub const MIGRATION_BUDGET: Duration = Duration::from_secs(60);
/// A copy that started within the budget may run this much past it: a copy
/// stopped half way is thrown away, so one large archive would never move.
pub const MIGRATION_GRACE: Duration = Duration::from_secs(120);
/// The most archives one pass moves.
pub const MIGRATIONS_PER_PASS: usize = 500;
/// The longest one pass spends evicting superseded host gate caches.
pub const GATE_EVICTION_BUDGET: Duration = Duration::from_secs(30);
/// Staging directory of a copy across volumes: `<prefix><archive>.partial`,
/// a partial name every reader skips.
const MIGRATING_PREFIX: &str = ".aethyme-migrating-";
/// Reasons named one by one in the pass report, at most.
const NAMED_REASONS: usize = 20;

/// Where one kind of data goes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Location {
    /// Host state (archives) or the host cache (gate caches).
    #[default]
    Host,
    /// An adopted storage root.
    Storage,
}

impl Location {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Storage => "storage",
        }
    }
}

/// `storage.toml` `[placement]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Placement {
    pub recovery_archives: Location,
    pub gate_caches: Location,
    /// The storage root to use; when absent, the one adopted root (several
    /// make the choice ambiguous, and placement stays on the host).
    pub storage_root: Option<PathBuf>,
}

/// The file as placement reads it: `[budget]` is read by
/// [`super::Budgets::load`].
#[derive(Debug, Default, Deserialize)]
struct PlacementFile {
    #[serde(default)]
    placement: Placement,
}

impl Placement {
    /// `<host state>/storage.toml` `[placement]`; an unreadable or invalid
    /// table keeps everything on the host and says why.
    pub fn load(host_state: &Path) -> (Self, Option<String>) {
        match fs::read_to_string(host_state.join(BUDGET_FILE)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => (Self::default(), None),
            Err(error) => (Self::default(), Some(format!("{BUDGET_FILE}: {error}"))),
            Ok(text) => match toml::from_str::<PlacementFile>(&text) {
                Ok(file) => (file.placement, None),
                Err(error) => (Self::default(), Some(format!("{BUDGET_FILE}: {error}"))),
            },
        }
    }

    fn configured(&self, data: Data) -> Location {
        match data {
            Data::RecoveryArchives => self.recovery_archives,
            Data::GateCaches => self.gate_caches,
        }
    }
}

/// What is being placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Data {
    RecoveryArchives,
    GateCaches,
}

impl Data {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecoveryArchives => "recovery_archives",
            Self::GateCaches => "gate_caches",
        }
    }
}

/// Why a storage placement fell back to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fallback {
    /// `[placement]` could not be read, or names a relative root.
    ConfigInvalid,
    /// No storage root was adopted.
    NoStorageRoot,
    /// Several were, and `storage_root` does not say which.
    AmbiguousStorageRoot,
    /// The root is missing, not a directory, or its volume is not mounted.
    Unavailable,
    /// The root is a symbolic link.
    Symlink,
    /// The root carries no adoption marker (or it is not a plain file).
    NotAdopted,
    /// The root or its marker belongs to another user.
    ForeignOwner,
    /// The root is on a network or unknown file system.
    NotLocal,
}

impl Fallback {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConfigInvalid => "config_invalid",
            Self::NoStorageRoot => "no_storage_root",
            Self::AmbiguousStorageRoot => "ambiguous_storage_root",
            Self::Unavailable => "unavailable",
            Self::Symlink => "symlink",
            Self::NotAdopted => "not_adopted",
            Self::ForeignOwner => "foreign_owner",
            Self::NotLocal => "not_local",
        }
    }

    /// The counter one fallback adds.
    pub fn counter(self) -> &'static str {
        match self {
            Self::ConfigInvalid => "storage_placement_fallback_config_invalid",
            Self::NoStorageRoot => "storage_placement_fallback_no_storage_root",
            Self::AmbiguousStorageRoot => "storage_placement_fallback_ambiguous_storage_root",
            Self::Unavailable => "storage_placement_fallback_unavailable",
            Self::Symlink => "storage_placement_fallback_symlink",
            Self::NotAdopted => "storage_placement_fallback_not_adopted",
            Self::ForeignOwner => "storage_placement_fallback_foreign_owner",
            Self::NotLocal => "storage_placement_fallback_not_local",
        }
    }
}

/// Where one kind of data goes now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub configured: Location,
    /// The qualified storage root, when the data goes there.
    pub storage_root: Option<PathBuf>,
    /// Why a storage placement fell back to the host.
    pub fallback: Option<Fallback>,
}

impl Resolved {
    fn host(configured: Location, fallback: Option<Fallback>) -> Self {
        Self {
            configured,
            storage_root: None,
            fallback,
        }
    }

    /// The storage root the data goes to, if any.
    pub fn on_storage(&self) -> Option<&Path> {
        self.storage_root.as_deref()
    }
}

#[cfg(unix)]
fn owned_by_this_user(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no preconditions and cannot fail.
    metadata.uid() == unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn owned_by_this_user(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(test)]
thread_local! {
    /// Tests: treat every file system as local or not.
    static LOCAL_FOR_TEST: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

fn local_file_system(path: &Path) -> bool {
    #[cfg(test)]
    if let Some(local) = LOCAL_FOR_TEST.with(std::cell::Cell::get) {
        return local;
    }
    crate::collaboration_state::filesystem_of(path).1
}

/// Whether `path` may hold placed data: a real directory (no symbolic link
/// at it) carrying a plain-file adoption marker, both owned by this user, on
/// a mounted, local file system.
pub fn qualify(path: &Path) -> Result<(), Fallback> {
    let metadata = fs::symlink_metadata(path).map_err(|_| Fallback::Unavailable)?;
    if metadata.file_type().is_symlink() {
        return Err(Fallback::Symlink);
    }
    if !metadata.is_dir() {
        return Err(Fallback::Unavailable);
    }
    // A directory left on the startup disk where a volume used to mount.
    if crate::verification::removable_volume_unavailable_reason(path).is_some() {
        return Err(Fallback::Unavailable);
    }
    let marker = fs::symlink_metadata(path.join(STORAGE_ROOT_MARKER))
        .ok()
        .filter(|marker| marker.file_type().is_file())
        .ok_or(Fallback::NotAdopted)?;
    if !owned_by_this_user(&metadata) || !owned_by_this_user(&marker) {
        return Err(Fallback::ForeignOwner);
    }
    if !local_file_system(path) {
        return Err(Fallback::NotLocal);
    }
    Ok(())
}

/// The storage root `placement` names, or the one adopted root.
pub fn select_storage_root(host_state: &Path, placement: &Placement) -> Result<PathBuf, Fallback> {
    match &placement.storage_root {
        Some(path) if !path.is_absolute() => Err(Fallback::ConfigInvalid),
        Some(path) => Ok(path.clone()),
        None => {
            let adopted = super::check::adopted_storage_roots(host_state);
            match adopted.as_slice() {
                [] => Err(Fallback::NoStorageRoot),
                [one] => Ok(one.clone()),
                _ => Err(Fallback::AmbiguousStorageRoot),
            }
        }
    }
}

/// Where `data` goes now, from `storage.toml` and the root's state. A
/// fallback is counted once per call (`storage_placement_fallback_<reason>`).
pub fn resolve(host_state: &Path, data: Data) -> Resolved {
    let (placement, error) = Placement::load(host_state);
    let resolved = if error.is_some() {
        // Nothing configured can be trusted, and the default is the host.
        Resolved::host(Location::Host, Some(Fallback::ConfigInvalid))
    } else {
        resolve_with(host_state, &placement, data)
    };
    if let Some(fallback) = resolved.fallback {
        crate::phase_telemetry::add(fallback.counter(), Duration::ZERO);
    }
    resolved
}

/// [`resolve`] for an already loaded `placement`, counting nothing.
pub fn resolve_with(host_state: &Path, placement: &Placement, data: Data) -> Resolved {
    let configured = placement.configured(data);
    if configured == Location::Host {
        return Resolved::host(configured, None);
    }
    let root = match select_storage_root(host_state, placement) {
        Ok(root) => root,
        Err(fallback) => return Resolved::host(configured, Some(fallback)),
    };
    match qualify(&root) {
        Ok(()) => Resolved {
            configured,
            storage_root: Some(root),
            fallback: None,
        },
        Err(fallback) => Resolved::host(configured, Some(fallback)),
    }
}

/// The storage roots recovery archives may be in: those recorded as having
/// received archives (`true`), then every other adopted root (`false`).
pub fn archive_storage_roots(host_state: &Path) -> Vec<(PathBuf, bool)> {
    let mut roots: Vec<(PathBuf, bool)> = recorded_archive_storage_roots(host_state)
        .into_iter()
        .map(|root| (root, true))
        .collect();
    for root in super::check::adopted_storage_roots(host_state) {
        if !roots.iter().any(|(known, _)| *known == root) {
            roots.push((root, false));
        }
    }
    roots
}

fn recorded_archive_storage_roots(host_state: &Path) -> Vec<PathBuf> {
    fs::read(host_state.join(ARCHIVE_STORAGE_ROOTS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Record that `root` holds (or is about to hold) recovery archives, before
/// the first one is written or moved there (a no-op when known).
///
/// Durable before it returns: written aside, synced, renamed in and the
/// directory synced. Recorders in several processes are serialized by an
/// exclusive lock on `<host state>/storage-archive-roots.lock`, and each
/// re-reads the file under it, so none drops a root another just added.
pub fn record_archive_storage_root(host_state: &Path, root: &Path) -> io::Result<()> {
    if recorded_archive_storage_roots(host_state)
        .iter()
        .any(|known| known == root)
    {
        return Ok(());
    }
    let _span = crate::phase_telemetry::span("storage_placement_record_root");
    fs::create_dir_all(host_state)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(host_state.join(ARCHIVE_STORAGE_ROOTS_LOCK_FILE))?;
    lock.lock()?;
    let mut known = recorded_archive_storage_roots(host_state);
    if known.iter().any(|known| known == root) {
        return Ok(());
    }
    known.push(root.to_path_buf());
    let path = host_state.join(ARCHIVE_STORAGE_ROOTS_FILE);
    let mut temporary = tempfile::NamedTempFile::new_in(host_state)?;
    io::Write::write_all(
        &mut temporary,
        &serde_json::to_vec(&known).map_err(io::Error::other)?,
    )?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path).map_err(|error| error.error)?;
    sync_directory(host_state)?;
    crate::phase_telemetry::add("storage_placement_root_recorded", Duration::ZERO);
    Ok(())
}

/// If `archive_root` is `<storage root>/recovery-archives/<key>` on an
/// adopted root, record that root (see [`record_archive_storage_root`]).
pub fn note_archive_root(host_state: &Path, archive_root: &Path) -> io::Result<()> {
    let Some(parent) = archive_root.parent() else {
        return Ok(());
    };
    let Some(root) = parent.parent() else {
        return Ok(());
    };
    if parent
        .file_name()
        .is_some_and(|name| name == RECOVERY_ARCHIVE_DIRECTORY)
        && root != host_state
        && root.join(STORAGE_ROOT_MARKER).is_file()
    {
        record_archive_storage_root(host_state, root)?;
    }
    Ok(())
}

/// What `storage status` shows of placement: configured and actual
/// location of each kind, why a storage placement fell back, the storage
/// root, the roots that received archives and the migration's totals.
/// Cheap: one small file read and a few metadata reads.
pub fn status(host_state: &Path) -> serde_json::Value {
    let (placement, error) = Placement::load(host_state);
    let describe = |data: Data| {
        let resolved = if error.is_some() {
            Resolved::host(Location::Host, Some(Fallback::ConfigInvalid))
        } else {
            resolve_with(host_state, &placement, data)
        };
        let placed = if resolved.on_storage().is_some() {
            Location::Storage
        } else {
            Location::Host
        };
        serde_json::json!({
            "configured": resolved.configured.as_str(),
            "placed": placed.as_str(),
            "fallback": resolved.fallback.map(Fallback::as_str),
        })
    };
    serde_json::json!({
        "recovery_archives": describe(Data::RecoveryArchives),
        "gate_caches": describe(Data::GateCaches),
        "storage_root": select_storage_root(host_state, &placement).ok(),
        "error": error,
        "archive_storage_roots": recorded_archive_storage_roots(host_state),
        "migration": MigrationState::load(host_state),
    })
}

/// What placement did in one storage worker pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementReport {
    /// Where new archives and gate caches go now (`host` or `storage`).
    pub recovery_archives: String,
    pub gate_caches: String,
    /// Why a storage placement fell back to the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_archives_fallback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_caches_fallback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_root: Option<PathBuf>,
    pub archives_migrated: u64,
    pub bytes_migrated: u64,
    /// Not moved this pass, for a reason a later pass may lift (partial,
    /// incomplete, its repository's lifecycle lock busy).
    pub archives_skipped: u64,
    pub archives_failed: u64,
    /// Archives still in host state once the pass stopped.
    pub archives_left: u64,
    /// Why the migration stopped: `done`, `deadline`, `per_pass_cap` or
    /// `lock_lost`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_stop: Option<String>,
    pub host_gate_caches_evicted: u64,
    pub host_gate_cache_bytes_evicted: u64,
    /// The first reasons archives were skipped or failed, and host gate
    /// caches kept: `<id>: <reason>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

impl PlacementReport {
    fn reason(&mut self, id: &str, why: &str) {
        if self.reasons.len() < NAMED_REASONS {
            self.reasons.push(format!("{id}: {why}"));
        }
    }
}

/// The migration's persisted progress.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationState {
    /// The last archive id (`<key>/<archive>`) a pass reached: the next one
    /// starts after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default)]
    pub archives_migrated_total: u64,
    #[serde(default)]
    pub bytes_migrated_total: u64,
    #[serde(default)]
    pub archives_failed_total: u64,
}

impl MigrationState {
    pub fn load(host_state: &Path) -> Self {
        fs::read(host_state.join(MIGRATION_STATE_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, host_state: &Path) -> io::Result<()> {
        write_atomically(
            &host_state.join(MIGRATION_STATE_FILE),
            &serde_json::to_vec(self).map_err(io::Error::other)?,
        )
    }
}

/// How one archive's move ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Moved {
    /// Moved; the bytes copied (0 for a rename).
    Done(u64),
    /// Left in place for now.
    Skipped(&'static str),
    /// Left in place; something is wrong with it or its destination.
    Failed(String),
    /// The pass ran out of time during the copy; nothing changed.
    OutOfTime,
}

#[cfg(test)]
thread_local! {
    /// Tests: stop a move at `after_copy` (copied, not yet in place) or
    /// `after_place` (in place, source not yet removed), as a crash would.
    pub(crate) static CRASH_FOR_TEST: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
    /// Tests: copy even when source and destination share a volume.
    pub(crate) static FORCE_COPY_FOR_TEST: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

fn crashed_for_test(point: &'static str) -> bool {
    #[cfg(test)]
    {
        CRASH_FOR_TEST.with(std::cell::Cell::get) == Some(point)
    }
    #[cfg(not(test))]
    {
        let _ = point;
        false
    }
}

fn force_copy() -> bool {
    #[cfg(test)]
    {
        FORCE_COPY_FOR_TEST.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// Archive ids (`<key>/<archive>`) in host state, in name order: complete
/// names only (no partial, staging, marker or dot-named entry).
fn host_archive_ids(source_root: &Path) -> Vec<String> {
    let mut ids = Vec::new();
    for key in super::read_names(source_root) {
        if key.starts_with('.') || key.starts_with(super::check::STAGED_DELETION_PREFIX) {
            continue;
        }
        let key_path = source_root.join(&key);
        if !fs::symlink_metadata(&key_path)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        {
            continue;
        }
        for name in super::read_names(&key_path) {
            if name.ends_with(super::KEEP_SUFFIX) || name.ends_with(super::DISMISS_SUFFIX) {
                continue;
            }
            ids.push(format!("{key}/{name}"));
        }
    }
    ids
}

/// Where a pass resumes in `ids` after `cursor`.
fn resume_index(ids: &[String], cursor: Option<&str>) -> usize {
    let Some(cursor) = cursor else {
        return 0;
    };
    ids.iter().position(|id| id.as_str() > cursor).unwrap_or(0)
}

/// A private directory, created when absent; never through a link.
fn ensure_private_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(io::Error::other(format!(
            "{} is not a real directory",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            crate::host_state::protect_host_state_path(path, true)?;
            // The new entry is durable before anything is placed in it.
            match path.parent() {
                Some(parent) => sync_directory(parent),
                None => Ok(()),
            }
        }
        Err(error) => Err(error),
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

/// The repository an archive's manifest names, if the manifest reads.
fn manifest_repository(archive: &Path) -> Option<PathBuf> {
    let manifest = archive.join("manifest.json");
    let small = fs::metadata(&manifest).is_ok_and(|metadata| metadata.len() <= 32 * 1024 * 1024);
    let value: serde_json::Value = small
        .then(|| fs::read(&manifest).ok())
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())?;
    value
        .get("repository")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
}

/// One entry of a tree as compared after a copy.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Directory,
    File { bytes: u64, sha256: [u8; 32] },
    Link(PathBuf),
}

/// Every entry under `root` by relative path, never following a link;
/// `Ok(None)` when `deadline` passed first.
fn tree_entries(root: &Path, deadline: Instant) -> io::Result<Option<BTreeMap<PathBuf, Entry>>> {
    use sha2::Digest as _;
    let mut entries = BTreeMap::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        if Instant::now() >= deadline {
            return Ok(None);
        }
        let path = root.join(&relative);
        let metadata = fs::symlink_metadata(&path)?;
        let entry = if metadata.file_type().is_symlink() {
            Entry::Link(fs::read_link(&path)?)
        } else if metadata.is_dir() {
            for child in fs::read_dir(&path)? {
                pending.push(relative.join(child?.file_name()));
            }
            Entry::Directory
        } else if metadata.is_file() {
            let mut file = fs::File::open(&path)?;
            let mut hasher = sha2::Sha256::new();
            let mut buffer = vec![0_u8; 1 << 16];
            let mut bytes = 0_u64;
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                bytes += read as u64;
                hasher.update(&buffer[..read]);
            }
            Entry::File {
                bytes,
                sha256: hasher.finalize().into(),
            }
        } else {
            return Err(io::Error::other(format!(
                "{} is not a file, directory or link",
                path.display()
            )));
        };
        entries.insert(relative, entry);
    }
    Ok(Some(entries))
}

/// Copy `source` to the new directory `destination`, never following a
/// link, syncing every file and directory; the bytes copied, or `Ok(None)`
/// when `deadline` passed first.
fn copy_tree(source: &Path, destination: &Path, deadline: Instant) -> io::Result<Option<u64>> {
    let mut bytes = 0_u64;
    let mut directories = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        if Instant::now() >= deadline {
            return Ok(None);
        }
        let from = source.join(&relative);
        let to = destination.join(&relative);
        let metadata = fs::symlink_metadata(&from)?;
        if metadata.file_type().is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(fs::read_link(&from)?, &to)?;
            #[cfg(not(unix))]
            return Err(io::Error::other("links are not copied on this platform"));
        } else if metadata.is_dir() {
            fs::create_dir(&to)?;
            for child in fs::read_dir(&from)? {
                pending.push(relative.join(child?.file_name()));
            }
            directories.push((to, metadata.permissions()));
        } else if metadata.is_file() {
            bytes += fs::copy(&from, &to)?;
            fs::File::open(&to)?.sync_all()?;
        } else {
            return Err(io::Error::other(format!(
                "{} is not a file, directory or link",
                from.display()
            )));
        }
    }
    // Deepest first, so each directory's entries are durable before it; its
    // mode last, so a read-only directory was still writable while filled.
    for (directory, permissions) in directories.into_iter().rev() {
        fs::set_permissions(&directory, permissions)?;
        sync_directory(&directory)?;
    }
    Ok(Some(bytes))
}

/// Copy the keep and dismissal markers of `source` beside `destination`,
/// replacing any there (each written aside, synced, then renamed in).
fn copy_markers(source: &Path, destination: &Path) -> io::Result<()> {
    for (from, to) in [
        (super::keep_marker(source), super::keep_marker(destination)),
        (
            super::dismiss_marker(source),
            super::dismiss_marker(destination),
        ),
    ] {
        match fs::symlink_metadata(&from) {
            Ok(metadata) if metadata.file_type().is_file() => {
                let staged = to.with_file_name(format!(
                    "{MIGRATING_PREFIX}{}.partial",
                    to.file_name()
                        .map(|name| name.to_string_lossy())
                        .unwrap_or_default()
                ));
                fs::copy(&from, &staged)?;
                fs::File::open(&staged)?.sync_all()?;
                fs::rename(&staged, &to)?;
            }
            Ok(_) => return Err(io::Error::other("a marker is not a plain file")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn remove_source_markers(source: &Path) -> io::Result<()> {
    for marker in [super::keep_marker(source), super::dismiss_marker(source)] {
        match fs::remove_file(&marker) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Remove the host copy once the archive is in place: renamed aside in
/// host state (atomically: readers never see it half gone), then deleted.
fn remove_source(host_state: &Path, id: &str, beat: &mut dyn FnMut()) -> io::Result<()> {
    let candidate = Candidate {
        class: Class::RecoveryArchives,
        root_kind: RootKind::HostState,
        root: host_state.to_path_buf(),
        relative: format!("{RECOVERY_ARCHIVE_DIRECTORY}/{id}"),
        bytes: 0,
        newest_ms: 0,
        reason: Reason::Expired,
        expires_ms: None,
        allow_unlanded: false,
    };
    // An interrupted deletion is left aside for the worker's sweep.
    evict::remove(&candidate, beat, None).map(drop)
}

/// Move one archive (see the module documentation).
fn migrate_one(
    host_state: &Path,
    destination_root: &Path,
    id: &str,
    hard_deadline: Instant,
    beat: &mut dyn FnMut(),
) -> Moved {
    let Some((key, name)) = id.split_once('/') else {
        return Moved::Failed("not an archive id".into());
    };
    if super::is_partial_archive_name(name) || name.starts_with('.') {
        return Moved::Skipped("being written");
    }
    let relative = format!("{RECOVERY_ARCHIVE_DIRECTORY}/{id}");
    if !evict::link_free(host_state, &relative) {
        return Moved::Failed("a symbolic link on its path".into());
    }
    let source = host_state.join(&relative);
    if !fs::symlink_metadata(&source).is_ok_and(|metadata| metadata.is_dir())
        || !fs::symlink_metadata(source.join("manifest.json"))
            .is_ok_and(|metadata| metadata.file_type().is_file())
    {
        return Moved::Skipped("incomplete (no manifest)");
    }
    // A restore, retirement or bound pass of its repository holds the
    // lifecycle lock while it reads or removes archives: never move under it.
    let _claim = match manifest_repository(&source) {
        Some(repository) if repository.join(".git").exists() => {
            match crate::file_lock::try_acquire_repository_lifecycle_lock_for_storage(&repository) {
                Ok(Some(claim)) => Some(claim),
                Ok(None) => return Moved::Skipped("its repository's lifecycle lock is busy"),
                Err(_) => return Moved::Skipped("its repository's lifecycle lock is unavailable"),
            }
        }
        // No repository there: no broker can restore from it.
        _ => None,
    };
    let destination_dir = destination_root.join(key);
    if let Err(error) = ensure_private_directory(&destination_dir) {
        return Moved::Failed(format!("destination: {error}"));
    }
    let destination = destination_dir.join(name);
    if let Err(error) = copy_markers(&source, &destination) {
        return Moved::Failed(format!("markers: {error}"));
    }
    match fs::symlink_metadata(&destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            // An earlier pass stopped after placing it: the source goes only
            // once both are proven identical.
            let _span = crate::phase_telemetry::span("storage_placement_migrate_reconcile");
            return match (
                tree_entries(&source, hard_deadline),
                tree_entries(&destination, hard_deadline),
            ) {
                (Ok(Some(left)), Ok(Some(right))) if left == right => {
                    match copy_markers(&source, &destination)
                        .and_then(|()| remove_source(host_state, id, beat))
                        .and_then(|()| remove_source_markers(&source))
                    {
                        Ok(()) => {
                            crate::phase_telemetry::add(
                                "storage_placement_archive_reconciled",
                                Duration::ZERO,
                            );
                            Moved::Done(0)
                        }
                        Err(error) => Moved::Failed(format!("remove the host copy: {error}")),
                    }
                }
                (Ok(None), _) | (_, Ok(None)) => Moved::OutOfTime,
                (Ok(Some(_)), Ok(Some(_))) => {
                    Moved::Failed("a different archive of that name is already there".into())
                }
                (Err(error), _) | (_, Err(error)) => Moved::Failed(format!("compare: {error}")),
            };
        }
        Ok(_) => return Moved::Failed("its destination is not a real directory".into()),
        Err(error) => return Moved::Failed(format!("destination: {error}")),
    }
    let same_volume =
        super::device_of(source.parent().unwrap_or(&source)) == super::device_of(&destination_dir);
    let renamed = same_volume && !force_copy() && {
        let _span = crate::phase_telemetry::span("storage_placement_migrate_rename");
        match fs::rename(&source, &destination) {
            Ok(()) => true,
            // Another volume after all (a bind or union mount): copy.
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => false,
            Err(error) => return Moved::Failed(format!("rename: {error}")),
        }
    };
    let copied = if renamed {
        0
    } else {
        let staging = destination_dir.join(format!("{MIGRATING_PREFIX}{name}.partial"));
        if fs::symlink_metadata(&staging).is_ok()
            && let Err(error) = evict::delete_tree(&staging, beat, None)
        {
            return Moved::Failed(format!("remove an earlier staging copy: {error}"));
        }
        let copy_span = crate::phase_telemetry::span("storage_placement_migrate_copy");
        let bytes = match copy_tree(&source, &staging, hard_deadline) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                crate::warn_unrecorded(
                    "remove an unfinished archive copy",
                    evict::delete_tree(&staging, beat, None).map(drop),
                );
                return Moved::OutOfTime;
            }
            Err(error) => {
                crate::warn_unrecorded(
                    "remove a failed archive copy",
                    evict::delete_tree(&staging, beat, None).map(drop),
                );
                return Moved::Failed(format!("copy: {error}"));
            }
        };
        drop(copy_span);
        let verify_span = crate::phase_telemetry::span("storage_placement_migrate_verify");
        let verdict = match (
            tree_entries(&source, hard_deadline),
            tree_entries(&staging, hard_deadline),
        ) {
            (Ok(Some(left)), Ok(Some(right))) if left == right => None,
            (Ok(None), _) | (_, Ok(None)) => Some(Moved::OutOfTime),
            (Ok(Some(_)), Ok(Some(_))) => Some(Moved::Failed(
                "the copy differs from the source (changed while copied?)".into(),
            )),
            (Err(error), _) | (_, Err(error)) => Some(Moved::Failed(format!("verify: {error}"))),
        };
        drop(verify_span);
        if let Some(outcome) = verdict {
            crate::warn_unrecorded(
                "remove an unverified archive copy",
                evict::delete_tree(&staging, beat, None).map(drop),
            );
            return outcome;
        }
        if crashed_for_test("after_copy") {
            return Moved::Failed("stopped after the copy (test)".into());
        }
        // `rename` onto an empty directory succeeds: check it is absent.
        if fs::symlink_metadata(&destination).is_ok() {
            return Moved::Failed("its destination appeared during the copy".into());
        }
        if let Err(error) = fs::rename(&staging, &destination) {
            return Moved::Failed(format!("place the copy: {error}"));
        }
        bytes
    };
    // The host copy goes only once the placed one is durable.
    if let Err(error) = sync_directory(&destination_dir) {
        return Moved::Failed(format!("sync the destination: {error}"));
    }
    if crashed_for_test("after_place") {
        return Moved::Failed("stopped after placing the copy (test)".into());
    }
    // Again, just before the source goes: a keep marker written during the
    // copy must not be lost with it.
    if let Err(error) = copy_markers(&source, &destination) {
        return Moved::Failed(format!("markers: {error}"));
    }
    if fs::symlink_metadata(&source).is_ok()
        && let Err(error) = remove_source(host_state, id, beat)
    {
        return Moved::Failed(format!("remove the host copy: {error}"));
    }
    // And once more after it: the markers sit beside the archive, so one
    // written while the source was removed is still there to copy.
    if let Err(error) = copy_markers(&source, &destination) {
        return Moved::Failed(format!("markers: {error}"));
    }
    if let Err(error) = remove_source_markers(&source) {
        return Moved::Failed(format!("remove the host markers: {error}"));
    }
    if let Some(parent) = source.parent() {
        crate::warn_unrecorded("sync the archive source", sync_directory(parent));
    }
    Moved::Done(copied)
}

/// Move host state's recovery archives to `storage_root` until `deadline`
/// (see the module documentation). `alive` refreshes the worker's heartbeat
/// and says whether the worker still holds its lock.
pub fn migrate_archives(
    host_state: &Path,
    storage_root: &Path,
    deadline: Instant,
    pace: Duration,
    alive: &mut dyn FnMut() -> bool,
    report: &mut PlacementReport,
) {
    let _span = crate::phase_telemetry::span("storage_placement_migrate");
    let source_root = host_state.join(RECOVERY_ARCHIVE_DIRECTORY);
    let destination_root = storage_root.join(RECOVERY_ARCHIVE_DIRECTORY);
    // Readers learn of the root before anything is moved or written there,
    // whether or not host state holds archives to move.
    if let Err(error) = record_archive_storage_root(host_state, storage_root) {
        crate::phase_telemetry::add("storage_placement_migrate_failed", Duration::ZERO);
        report.archives_failed += 1;
        report.reason(RECOVERY_ARCHIVE_DIRECTORY, &error.to_string());
        return;
    }
    if !fs::symlink_metadata(&source_root)
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    {
        report.migration_stop = Some("done".into());
        return;
    }
    if let Err(error) = ensure_private_directory(&destination_root) {
        crate::phase_telemetry::add("storage_placement_migrate_failed", Duration::ZERO);
        report.archives_failed += 1;
        report.reason(RECOVERY_ARCHIVE_DIRECTORY, &error.to_string());
        return;
    }
    let mut state = MigrationState::load(host_state);
    let ids = {
        let _span = crate::phase_telemetry::span("storage_placement_migrate_list");
        host_archive_ids(&source_root)
    };
    let start = resume_index(&ids, state.cursor.as_deref());
    let mut stop = "done";
    let mut reached = 0;
    let mut moved = 0;
    for (index, id) in ids.iter().cycle().skip(start).take(ids.len()).enumerate() {
        if Instant::now() >= deadline {
            stop = "deadline";
            break;
        }
        if moved >= MIGRATIONS_PER_PASS {
            stop = "per_pass_cap";
            break;
        }
        if !alive() {
            stop = "lock_lost";
            break;
        }
        reached = index + 1;
        match migrate_one(
            host_state,
            &destination_root,
            id,
            deadline + MIGRATION_GRACE,
            &mut || {
                alive();
            },
        ) {
            Moved::Done(bytes) => {
                moved += 1;
                crate::phase_telemetry::add("storage_placement_archive_migrated", Duration::ZERO);
                report.archives_migrated += 1;
                report.bytes_migrated += bytes;
                state.archives_migrated_total += 1;
                state.bytes_migrated_total += bytes;
            }
            Moved::Skipped(why) => {
                crate::phase_telemetry::add("storage_placement_archive_skipped", Duration::ZERO);
                report.archives_skipped += 1;
                report.reason(id, why);
            }
            Moved::Failed(why) => {
                crate::phase_telemetry::add("storage_placement_archive_failed", Duration::ZERO);
                report.archives_failed += 1;
                state.archives_failed_total += 1;
                report.reason(id, &why);
            }
            Moved::OutOfTime => {
                crate::phase_telemetry::add(
                    "storage_placement_archive_out_of_time",
                    Duration::ZERO,
                );
                report.reason(id, "the pass ran out of time during the copy");
                stop = "deadline";
                break;
            }
        }
        state.cursor = Some(id.clone());
        if !pace.is_zero() {
            std::thread::sleep(pace);
        }
    }
    if stop == "done" && reached == ids.len() {
        // A whole rotation: the next pass starts from the first name.
        state.cursor = None;
    }
    report.migration_stop = Some(stop.into());
    report.archives_left = host_archive_ids(&source_root)
        .iter()
        .filter(|id| {
            id.split_once('/')
                .is_some_and(|(_, name)| !super::is_partial_archive_name(name))
        })
        .count() as u64;
    crate::phase_telemetry::attach(
        "storage_placement",
        serde_json::json!({
            "archives_migrated": report.archives_migrated,
            "bytes_migrated": report.bytes_migrated,
            "archives_skipped": report.archives_skipped,
            "archives_failed": report.archives_failed,
            "archives_left": report.archives_left,
            "migration_stop": stop,
        }),
    );
    crate::warn_unrecorded("record the archive migration", state.save(host_state));
    if report.archives_migrated > 0 {
        super::note_written(host_state, RootKind::HostState, RECOVERY_ARCHIVE_DIRECTORY);
        super::note_written(host_state, RootKind::Storage, RECOVERY_ARCHIVE_DIRECTORY);
    }
}

/// While gate caches are placed on a storage root, remove the host cache's
/// gate caches: each under its exclusive lease (a gate still running on one
/// keeps it), paced, until `deadline`. Never moved: a cache is rebuilt.
pub fn evict_superseded_host_gate_caches(
    ledger: &mut Ledger,
    registry: &Path,
    deadline: Instant,
    pace: Duration,
    alive: &mut dyn FnMut() -> bool,
    report: &mut PlacementReport,
) {
    let _span = crate::phase_telemetry::span("storage_placement_host_gate_cache_evict");
    let leases = evict::leased_gate_caches(registry);
    let now = super::now_ms();
    let candidates: Vec<Candidate> = ledger
        .items
        .iter()
        .filter(|item| item.root_kind == RootKind::HostCache && item.class == Class::GateCache)
        .flat_map(|item: &Item| {
            item.children
                .iter()
                .map(|child| evict::child_candidate(item, child, Reason::Expired))
                .collect::<Vec<_>>()
        })
        .collect();
    for candidate in candidates {
        if Instant::now() >= deadline || !alive() {
            break;
        }
        if let Some(why) = evict::refuse(&candidate, leases.as_ref(), now) {
            crate::phase_telemetry::add("storage_placement_host_gate_cache_kept", Duration::ZERO);
            report.reason(&candidate.label(), &why);
            continue;
        }
        let mut beat = || {
            alive();
        };
        match evict::remove_protected(&candidate, registry, &mut beat, None) {
            Ok(_) => {
                crate::phase_telemetry::add(
                    "storage_placement_host_gate_cache_evicted",
                    Duration::ZERO,
                );
                report.host_gate_caches_evicted += 1;
                report.host_gate_cache_bytes_evicted += candidate.bytes;
                super::check::forget(ledger, &candidate);
            }
            Err(why) => {
                crate::phase_telemetry::add(
                    "storage_placement_host_gate_cache_kept",
                    Duration::ZERO,
                );
                report.reason(&candidate.label(), &why);
            }
        }
        if !pace.is_zero() {
            std::thread::sleep(pace);
        }
    }
}

/// One storage worker pass's placement work: resolve both placements, move
/// archives while they go to a storage root, and evict superseded host gate
/// caches while those do. `None` when nothing is placed off the host by
/// configuration (the default): the pass then does nothing here.
pub fn run(
    host_state: &Path,
    ledger: &mut Ledger,
    registry: &Path,
    pace: Duration,
    alive: &mut dyn FnMut() -> bool,
) -> Option<PlacementReport> {
    let _span = crate::phase_telemetry::span("storage_placement");
    let archives = resolve(host_state, Data::RecoveryArchives);
    let gates = resolve(host_state, Data::GateCaches);
    let configured = |resolved: &Resolved| {
        resolved.configured == Location::Storage || resolved.fallback.is_some()
    };
    if !configured(&archives) && !configured(&gates) {
        return None;
    }
    let placed = |resolved: &Resolved| {
        if resolved.on_storage().is_some() {
            Location::Storage
        } else {
            Location::Host
        }
        .as_str()
        .to_string()
    };
    let mut report = PlacementReport {
        recovery_archives: placed(&archives),
        gate_caches: placed(&gates),
        recovery_archives_fallback: archives.fallback.map(|reason| reason.as_str().into()),
        gate_caches_fallback: gates.fallback.map(|reason| reason.as_str().into()),
        storage_root: archives
            .storage_root
            .clone()
            .or_else(|| gates.storage_root.clone()),
        ..PlacementReport::default()
    };
    if let Some(root) = archives.on_storage() {
        migrate_archives(
            host_state,
            root,
            Instant::now() + MIGRATION_BUDGET,
            pace,
            alive,
            &mut report,
        );
    }
    if gates.on_storage().is_some() {
        evict_superseded_host_gate_caches(
            ledger,
            registry,
            Instant::now() + GATE_EVICTION_BUDGET,
            pace,
            alive,
            &mut report,
        );
    }
    Some(report)
}

#[cfg(test)]
pub(crate) fn set_local_for_test(local: Option<bool>) {
    LOCAL_FOR_TEST.with(|cell| cell.set(local));
}

#[cfg(test)]
mod tests;
