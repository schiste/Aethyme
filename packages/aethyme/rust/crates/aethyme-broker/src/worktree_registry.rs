//! Durable, bounded discovery of exact worktree roots, independent of placement.
//!
//! This database shares the host-operation coordination domain. Explicit private
//! host-operation database overrides also get a private registry. Records are
//! hints, never deletion authority; legacy roots without saved volume identity
//! remain diagnostic until independently adopted while their origin is present.

use std::fs::{self, Metadata, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::WorktreeRootSource;
use crate::broker::{WORKTREE_ROOT_MARKER, WORKTREE_ROOT_SCHEMA_VERSION, WorktreeRootMarker};

const MAX_RECORD_BYTES: usize = 8192;
const MAX_RECORDS: usize = 8192;
const PAGE_RECORDS: usize = 64;
const MAX_INVENTORY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RootRecord {
    schema_version: u32,
    pub(crate) root: PathBuf,
    pub(crate) repository_key: String,
    pub(crate) repository_root: PathBuf,
    source: WorktreeRootSource,
    identity: Option<DirectoryIdentity>,
    origin_identity: Option<DirectoryIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
    filesystem: Vec<u8>,
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn directory_identity(path: &Path, metadata: &Metadata) -> io::Result<Option<DirectoryIdentity>> {
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(invalid)?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: path is NUL terminated and stat points to writable storage of the
    // exact type statfs requires. Read it only after a successful call.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful statfs initialized the structure (zeroed above too).
    let stat = unsafe { stat.assume_init() };
    // fsid_t exposes no portable field accessor. Its initialized representation
    // is used only for equality on this host, never interpreted as a number.
    // SAFETY: the slice covers only the initialized f_fsid field's bytes.
    let filesystem = unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!(stat.f_fsid).cast::<u8>(),
            std::mem::size_of_val(&stat.f_fsid),
        )
    }
    .to_vec();
    Ok(Some(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        filesystem,
    }))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn directory_identity(_path: &Path, _metadata: &Metadata) -> io::Result<Option<DirectoryIdentity>> {
    Ok(None)
}

fn real_directory(path: &Path) -> io::Result<Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(invalid(format!(
            "{} is not a real directory",
            path.display()
        )));
    }
    Ok(metadata)
}

fn registry_directory(host_operation_database: &Path) -> io::Result<PathBuf> {
    let parent = host_operation_database
        .parent()
        .ok_or_else(|| invalid("host operation database has no parent"))?;
    let name = host_operation_database
        .file_name()
        .ok_or_else(|| invalid("host operation database has no filename"))?;
    Ok(parent.join(format!(
        "worktree-roots-{:x}",
        Sha256::digest(name.as_encoded_bytes())
    )))
}

pub(crate) fn registry_path(host_operation_database: &Path) -> io::Result<PathBuf> {
    Ok(registry_directory(host_operation_database)?.join("registry.sqlite3"))
}

fn record_key(root: &Path) -> String {
    format!("{:x}", Sha256::digest(root.as_os_str().as_encoded_bytes()))
}

fn regular_file(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid("worktree registry is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(invalid("worktree registry has multiple hard links"));
        }
    }
    Ok(())
}

fn registry_outside_root(host_database: &Path, root: &Path) -> io::Result<()> {
    let registry = crate::broker::lifecycle::canonical_prefix(&registry_directory(host_database)?)
        .map_err(invalid)?;
    if registry.starts_with(root) {
        return Err(invalid(
            "worktree registry must reside outside the root it coordinates",
        ));
    }
    Ok(())
}

/// One stable host-domain lock excludes root admission for the entire orphan
/// removal. It is never unlinked or truncated. Contention refuses admission
/// promptly; a crashed remover releases the OS lock without stale tombstones.
fn registry_lock(
    host_database: &Path,
    create: bool,
) -> io::Result<crate::file_lock::ExclusiveFileLock> {
    let path = registry_path(host_database)?.with_extension("lock");
    if create {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("registry lock has no parent"))?;
        fs::create_dir_all(parent)?;
        real_directory(parent)?;
        crate::host_state::protect_host_state_path(parent, true)?;
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(&path)?;
    regular_file(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid("registry lock changed type"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let named = fs::symlink_metadata(&path)?;
        if metadata.nlink() != 1 || named.ino() != metadata.ino() || named.dev() != metadata.dev() {
            return Err(invalid(
                "registry lock object changed or has multiple links",
            ));
        }
    }
    let claim = crate::file_lock::ExclusiveFileLock::try_acquire(file)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "worktree history is being updated or reclaimed; retry after GC completes",
        )
    })?;
    regular_file(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let named = fs::symlink_metadata(&path)?;
        if named.nlink() != 1 || named.ino() != metadata.ino() || named.dev() != metadata.dev() {
            return Err(invalid(
                "registry lock object changed while acquiring exclusion",
            ));
        }
    }
    Ok(claim)
}

fn open_registry(host_database: &Path, create: bool) -> io::Result<Connection> {
    let path = registry_path(host_database)?;
    if create {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("registry has no parent"))?;
        fs::create_dir_all(parent)?;
        real_directory(parent)?;
        crate::host_state::protect_host_state_path(parent, true)?;
    }
    match regular_file(&path) {
        Ok(()) => {}
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            }
            match options.open(&path) {
                Ok(file) => file.sync_all()?,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => regular_file(&path)?,
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    }
    // SQLite NOFOLLOW rejects symlinks in parent components too (notably
    // macOS /var -> /private/var). Resolve only the verified private parent;
    // never canonicalize the database leaf and thereby accept a symlink.
    let path = fs::canonicalize(
        path.parent()
            .ok_or_else(|| invalid("registry has no parent"))?,
    )?
    .join("registry.sqlite3");
    let flags = if create {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection = Connection::open_with_flags(&path, flags).map_err(invalid)?;
    connection
        .busy_timeout(Duration::from_millis(25))
        .map_err(invalid)?;
    if create {
        // The registry itself must not become another unbounded artifact.
        // DELETE journaling avoids WAL growth pinned by a slow inventory reader.
        connection
            .execute_batch(
                "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA max_page_count=8192;",
            )
            .map_err(invalid)?;
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(invalid)?;
        match version {
            0 => connection.execute_batch("BEGIN IMMEDIATE;
                CREATE TABLE IF NOT EXISTS roots (key TEXT PRIMARY KEY, payload TEXT NOT NULL CHECK(length(CAST(payload AS BLOB)) <= 8192));
                CREATE TABLE IF NOT EXISTS registry_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                PRAGMA user_version=1; COMMIT;").map_err(invalid)?,
            1 => {},
            _ => return Err(invalid("unsupported worktree registry schema")),
        }
    }
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(invalid)?;
    if version != 1 {
        return Err(invalid("unsupported worktree registry schema"));
    }
    regular_file(&path)?;
    Ok(connection)
}

pub(crate) fn read_marker(root: &Path) -> io::Result<WorktreeRootMarker> {
    real_directory(root)?;
    let path = root.join(WORKTREE_ROOT_MARKER);
    regular_file(&path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("worktree marker changed type"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    decode_marker(&bytes)
}

fn decode_marker(bytes: &[u8]) -> io::Result<WorktreeRootMarker> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(invalid("worktree marker exceeds its byte limit"));
    }
    let marker: WorktreeRootMarker = serde_json::from_slice(bytes).map_err(invalid)?;
    if marker.schema_version != WORKTREE_ROOT_SCHEMA_VERSION
        || marker.repository_key.is_empty()
        || !marker.repository_root.is_absolute()
    {
        return Err(invalid("worktree marker identity or schema is invalid"));
    }
    Ok(marker)
}

fn marker_matches(record: &RootRecord) -> io::Result<()> {
    let marker = read_marker(&record.root)?;
    marker_owner_matches(record, &marker)
}

fn marker_owner_matches(record: &RootRecord, marker: &WorktreeRootMarker) -> io::Result<()> {
    if marker.repository_key != record.repository_key
        || marker.repository_root != record.repository_root
    {
        return Err(invalid("worktree marker differs from registered owner"));
    }
    Ok(())
}

fn decode(key: &str, payload: &str) -> io::Result<RootRecord> {
    if payload.len() > MAX_RECORD_BYTES {
        return Err(invalid("registry record exceeds byte limit"));
    }
    let record: RootRecord = serde_json::from_str(payload).map_err(invalid)?;
    if record.schema_version != 1
        || !record.root.is_absolute()
        || !record.repository_root.is_absolute()
        || record.repository_key.is_empty()
        || record_key(&record.root) != key
    {
        return Err(invalid("registry record identity or schema is invalid"));
    }
    Ok(record)
}

/// A missing path counts only when its nearest existing real ancestor is on the
/// exact filesystem recorded while the path existed. An unmounted/replaced
/// external volume, symlink or unsupported platform leaves the record unknown.
fn absent_on_original_filesystem(
    path: &Path,
    identity: Option<&DirectoryIdentity>,
) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let identity = identity.ok_or_else(|| invalid("no saved filesystem identity"))?;
    for ancestor in path.ancestors().skip(1) {
        match real_directory(ancestor) {
            Ok(metadata) => {
                let actual = directory_identity(ancestor, &metadata)?
                    .ok_or_else(|| invalid("unsupported filesystem identity"))?;
                if actual.device == identity.device && actual.filesystem == identity.filesystem {
                    return Ok(true);
                }
                return Err(invalid("original filesystem is unavailable or replaced"));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Err(invalid("no inspectable ancestor"))
}

fn root_still_present(record: &RootRecord) -> io::Result<bool> {
    let metadata = match real_directory(&record.root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return absent_on_original_filesystem(&record.root, record.identity.as_ref())
                .map(|absent| !absent);
        }
        Err(error) => return Err(error),
    };
    if record.identity.is_none() || directory_identity(&record.root, &metadata)? != record.identity
    {
        return Err(invalid("registered root object or filesystem changed"));
    }
    marker_matches(record)?;
    Ok(true)
}

pub(crate) fn origin_is_missing(record: &RootRecord) -> io::Result<bool> {
    absent_on_original_filesystem(&record.repository_root, record.origin_identity.as_ref())
}

// Materialize small pages and release the statement before any filesystem IO.
fn page(connection: &Connection, after: &str) -> io::Result<Vec<(String, String)>> {
    let mut statement = connection.prepare("SELECT key, CASE WHEN length(CAST(payload AS BLOB)) <= 8192 THEN payload ELSE NULL END FROM roots WHERE key > ?1 ORDER BY key LIMIT 64").map_err(invalid)?;
    let rows = statement
        .query_map([after], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(invalid)?;
    let mut result = Vec::new();
    for row in rows {
        let (key, payload) = row.map_err(invalid)?;
        if key.len() != 64 {
            return Err(invalid("invalid registry key"));
        }
        result.push((
            key,
            payload.ok_or_else(|| invalid("oversized registry record"))?,
        ));
    }
    Ok(result)
}

/// Called before admitting a worktree. Registry failure refuses admission;
/// silently creating undiscoverable storage would recreate the original leak.
pub(crate) fn register(
    host_database: &Path,
    root: &Path,
    marker: &WorktreeRootMarker,
    source: WorktreeRootSource,
) -> io::Result<()> {
    register_with_limit(host_database, root, marker, source, MAX_RECORDS)
}

fn register_with_limit(
    host_database: &Path,
    root: &Path,
    marker: &WorktreeRootMarker,
    source: WorktreeRootSource,
    limit: usize,
) -> io::Result<()> {
    let root = fs::canonicalize(root)?;
    registry_outside_root(host_database, &root)?;
    let _admission = registry_lock(host_database, true)?;
    let actual = read_marker(&root)?;
    if actual.schema_version != marker.schema_version
        || actual.repository_key != marker.repository_key
        || actual.repository_root != marker.repository_root
    {
        return Err(invalid("worktree marker differs from admission owner"));
    }
    let record = RootRecord {
        schema_version: 1,
        root: root.clone(),
        repository_key: marker.repository_key.clone(),
        repository_root: marker.repository_root.clone(),
        source,
        identity: directory_identity(&root, &real_directory(&root)?)?,
        origin_identity: directory_identity(
            &marker.repository_root,
            &real_directory(&marker.repository_root)?,
        )?,
    };
    let payload = serde_json::to_string(&record).map_err(invalid)?;
    if payload.len() > MAX_RECORD_BYTES {
        return Err(invalid(
            "worktree root identity exceeds registry byte limit",
        ));
    }
    let mut connection = open_registry(host_database, true)?;
    let cursor: String = connection
        .query_row(
            "SELECT value FROM registry_meta WHERE key='prune_cursor'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(invalid)?
        .unwrap_or_default();
    let mut entries = page(&connection, &cursor)?;
    if entries.is_empty() && !cursor.is_empty() {
        entries = page(&connection, "")?;
    }
    let removable: Vec<_> = entries
        .iter()
        .filter(|(key, payload)| {
            decode(key, payload).is_ok_and(|old| {
                absent_on_original_filesystem(&old.root, old.identity.as_ref()).unwrap_or(false)
            })
        })
        .collect();
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(invalid)?;
    for (key, old_payload) in removable {
        let old = decode(key, old_payload)?;
        // Re-check absence under the writer lock, and compare the exact prior
        // payload so an upsert by a concurrent registrar cannot be pruned.
        if absent_on_original_filesystem(&old.root, old.identity.as_ref())? {
            transaction
                .execute(
                    "DELETE FROM roots WHERE key=?1 AND payload=?2",
                    params![key, old_payload],
                )
                .map_err(invalid)?;
        }
    }
    let key = record_key(&root);
    let count: usize = transaction
        .query_row("SELECT count(*) FROM roots", [], |row| row.get(0))
        .map_err(invalid)?;
    let exists: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM roots WHERE key=?1)",
            [&key],
            |row| row.get(0),
        )
        .map_err(invalid)?;
    let next = entries.last().map_or("", |(key, _)| key.as_str());
    transaction.execute("INSERT INTO registry_meta(key,value) VALUES('prune_cursor',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [next]).map_err(invalid)?;
    if count >= limit && !exists {
        // Persist maintenance progress even when this admission is denied.
        // Otherwise a full registry would retry its first page forever.
        transaction.commit().map_err(invalid)?;
        return Err(invalid(
            "worktree registry is full; retry bounded maintenance or inspect retained root history before admitting more storage",
        ));
    }
    marker_matches(&record)?;
    if directory_identity(&root, &real_directory(&root)?)? != record.identity {
        return Err(invalid("root changed during registration"));
    }
    transaction.execute("INSERT INTO roots(key,payload) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET payload=excluded.payload", params![key, payload]).map_err(invalid)?;
    transaction.commit().map_err(invalid)
}

#[derive(Debug, Default)]
pub(crate) struct Inventory {
    pub(crate) roots: Vec<RootRecord>,
    pub(crate) issues: Vec<String>,
    pub(crate) complete: bool,
}

pub(crate) fn inventory(host_database: &Path, deadline: Option<Instant>) -> Inventory {
    let mut result = Inventory {
        complete: true,
        ..Inventory::default()
    };
    let inspected = (|| -> io::Result<()> {
        let connection = open_registry(host_database, false)?;
        let mut cursor = String::new();
        let mut bytes = 0;
        let mut count = 0;
        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(invalid("worktree history inspection budget expired"));
            }
            let entries = page(&connection, &cursor)?;
            if entries.is_empty() {
                break;
            }
            for (key, payload) in &entries {
                count += 1;
                bytes += payload.len();
                if count > MAX_RECORDS
                    || bytes > MAX_INVENTORY_BYTES
                    || deadline.is_some_and(|deadline| Instant::now() >= deadline)
                {
                    return Err(invalid("worktree history inspection budget expired"));
                }
                let record = decode(key, payload)?;
                match root_still_present(&record) {
                    Ok(true) => result.roots.push(record),
                    Ok(false) => {}
                    Err(error) => {
                        result.complete = false;
                        result.issues.push(format!(
                            "registered root {} uninspected: {error}",
                            record.root.display()
                        ));
                    }
                }
            }
            cursor = entries.last().expect("nonempty page").0.clone();
            if entries.len() < PAGE_RECORDS {
                break;
            }
        }
        Ok(())
    })();
    if let Err(error) = inspected {
        result.complete = false;
        result
            .issues
            .push(format!("worktree root history unavailable: {error}"));
    }
    result.roots.sort_by(|a, b| a.root.cmp(&b.root));
    result
}

pub(crate) fn ownership_fingerprint(record: &RootRecord) -> io::Result<String> {
    if !root_still_present(record)? || !origin_is_missing(record)? {
        return Err(invalid("root is no longer an orphan"));
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(record).map_err(invalid)?)
    ))
}

#[cfg(test)]
fn revalidate(host_database: &Path, root: &Path, expected: &str) -> io::Result<()> {
    let connection = open_registry(host_database, false)?;
    let key = record_key(root);
    let payload: Option<String> = connection.query_row("SELECT CASE WHEN length(CAST(payload AS BLOB)) <= 8192 THEN payload ELSE NULL END FROM roots WHERE key=?1", [&key], |row| row.get(0)).optional().map_err(invalid)?.flatten();
    let record = decode(
        &key,
        &payload.ok_or_else(|| invalid("root no longer registered"))?,
    )?;
    if ownership_fingerprint(&record)? != expected {
        return Err(invalid("registered orphan evidence changed"));
    }
    Ok(())
}

pub(crate) struct RemovalClaim {
    _lock: crate::file_lock::ExclusiveFileLock,
    pub(crate) already_absent: bool,
    tree: Option<crate::gc::PinnedTreeRemoval>,
    record: RootRecord,
}

impl std::fmt::Debug for RemovalClaim {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemovalClaim")
            .field("already_absent", &self.already_absent)
            .finish_non_exhaustive()
    }
}

impl RemovalClaim {
    /// Delete through the directory handle whose identity this claim proved.
    /// Keep this claim alive through the caller's durable journal update.
    pub(crate) fn remove(
        &mut self,
        deadline: Option<Instant>,
    ) -> io::Result<crate::gc::TreeRemoval> {
        if self.already_absent {
            return Ok(crate::gc::TreeRemoval::Complete);
        }
        let record = &self.record;
        self.tree
            .take()
            .ok_or_else(|| invalid("removal claim has already consumed its directory handle"))?
            .remove_verified_witness(WORKTREE_ROOT_MARKER, deadline, |bytes| {
                marker_owner_matches(record, &decode_marker(bytes)?)?;
                if !origin_is_missing(record)? {
                    return Err(invalid("owning repository reappeared before deletion"));
                }
                Ok(())
            })
    }
}

#[derive(Debug)]
pub(crate) enum ClaimError {
    /// The old authorization has conclusively lost its ownership premise.
    Revoked(String),
    /// The root is retained; a fresh plan may not rediscover missing history.
    HistoryLost(String),
    /// Inspection or coordination could not finish. Keep durable work pending.
    Unavailable(io::Error),
}

impl From<io::Error> for ClaimError {
    fn from(error: io::Error) -> Self {
        Self::Unavailable(error)
    }
}

pub(crate) fn claim_for_removal(
    host_database: &Path,
    root: &Path,
    expected: &str,
) -> Result<RemovalClaim, ClaimError> {
    registry_outside_root(host_database, root)?;
    let claim = registry_lock(host_database, false)?;
    let connection = open_registry(host_database, false)?;
    let key = record_key(root);
    let payload: Option<String> = connection.query_row("SELECT CASE WHEN length(CAST(payload AS BLOB)) <= 8192 THEN payload ELSE NULL END FROM roots WHERE key=?1", [&key], |row| row.get(0)).optional().map_err(invalid)?.flatten();
    let payload = payload.ok_or_else(|| {
        ClaimError::HistoryLost("registered ownership record is missing or oversized".into())
    })?;
    let record =
        decode(&key, &payload).map_err(|error| ClaimError::HistoryLost(error.to_string()))?;
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&record).map_err(invalid)?)
    );
    if fingerprint != expected {
        return Err(ClaimError::Revoked(
            "registered ownership record changed".into(),
        ));
    }
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if absent_on_original_filesystem(root, record.identity.as_ref())? {
                return Ok(RemovalClaim {
                    _lock: claim,
                    already_absent: true,
                    tree: None,
                    record,
                });
            }
            return Err(ClaimError::Unavailable(invalid(
                "root absence could not be proved",
            )));
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ClaimError::Revoked(
            "registered root was replaced by another object".into(),
        ));
    }
    if record.identity.is_none() {
        return Err(invalid("no supported root filesystem identity").into());
    }
    if directory_identity(root, &metadata)? != record.identity {
        return Err(ClaimError::Revoked(
            "registered root object or filesystem changed".into(),
        ));
    }
    if let Err(error) = marker_matches(&record) {
        return Err(match error.kind() {
            io::ErrorKind::InvalidData | io::ErrorKind::NotFound => {
                ClaimError::Revoked(format!("owner marker changed: {error}"))
            }
            _ => ClaimError::Unavailable(error),
        });
    }
    if !origin_is_missing(&record)? {
        return Err(ClaimError::Revoked("owning repository reappeared".into()));
    }
    let tree = crate::gc::PinnedTreeRemoval::open(root)?;
    let pinned: crate::gc::TreeObjectIdentity = tree.object_identity()?;
    let saved = record
        .identity
        .as_ref()
        .ok_or_else(|| invalid("no supported root filesystem identity"))?;
    if pinned.device != saved.device
        || pinned.inode != saved.inode
        || tree.filesystem_identity()? != saved.filesystem
    {
        return Err(ClaimError::Revoked(
            "registered root changed before its removal handle was pinned".into(),
        ));
    }
    Ok(RemovalClaim {
        _lock: claim,
        already_absent: false,
        tree: Some(tree),
        record,
    })
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;

    struct Fixture {
        _owned: tempfile::TempDir,
        database: PathBuf,
        origin: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let owned = tempfile::tempdir().unwrap();
            let database = owned.path().join("private-state/host-operations.db");
            let origin = owned.path().join("origin");
            fs::create_dir(&origin).unwrap();
            Self {
                _owned: owned,
                database,
                origin,
            }
        }

        fn root(&self, name: &str) -> (PathBuf, WorktreeRootMarker) {
            let root = self._owned.path().join(name);
            fs::create_dir_all(&root).unwrap();
            let marker = WorktreeRootMarker {
                schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
                repository_key: name.replace('/', "-"),
                repository_root: self.origin.clone(),
            };
            fs::write(
                root.join(WORKTREE_ROOT_MARKER),
                serde_json::to_vec(&marker).unwrap(),
            )
            .unwrap();
            (root.canonicalize().unwrap(), marker)
        }

        fn register(&self, root: &Path, marker: &WorktreeRootMarker) {
            register(
                &self.database,
                root,
                marker,
                WorktreeRootSource::LibraryOverride,
            )
            .unwrap();
        }
    }

    #[test]
    fn exact_historical_roots_survive_placement_changes_and_owner_disappearance() {
        let fixture = Fixture::new();
        let (first, a) = fixture.root("old-placement/exact-root");
        let (second, b) = fixture.root("new-placement/different-root");
        fixture.register(&first, &a);
        fixture.register(&second, &b);
        fs::remove_dir(&fixture.origin).unwrap();
        let found = inventory(&fixture.database, None);
        assert!(found.complete, "{:?}", found.issues);
        assert_eq!(found.roots.len(), 2);
        let record = found
            .roots
            .iter()
            .find(|record| record.root == first)
            .unwrap();
        let fingerprint = ownership_fingerprint(record).unwrap();
        revalidate(&fixture.database, &first, &fingerprint).unwrap();
        assert!(
            registry_path(&fixture.database)
                .unwrap()
                .starts_with(fixture._owned.path())
        );
        // A restored origin revokes the saved deletion proof.
        fs::create_dir(&fixture.origin).unwrap();
        assert!(revalidate(&fixture.database, &first, &fingerprint).is_err());
        assert!(first.is_dir());
        assert!(second.is_dir());
    }

    #[test]
    fn replacing_a_claimed_root_cannot_redirect_removal_to_the_new_directory() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fs::write(root.join("original.txt"), "keep original").unwrap();
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let record = inventory(&fixture.database, None).roots.remove(0);
        let fingerprint = ownership_fingerprint(&record).unwrap();
        let mut claim = claim_for_removal(&fixture.database, &root, &fingerprint).unwrap();

        let retained = root.with_extension("retained");
        fs::rename(&root, &retained).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("replacement.txt"), "keep replacement").unwrap();
        fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();

        assert!(claim.remove(None).is_err());
        assert_eq!(
            fs::read_to_string(retained.join("original.txt")).unwrap(),
            "keep original"
        );
        assert_eq!(
            fs::read_to_string(root.join("replacement.txt")).unwrap(),
            "keep replacement"
        );
        assert!(retained.join(WORKTREE_ROOT_MARKER).is_file());
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
        assert!(
            claim.remove(None).is_err(),
            "a consumed claim cannot be replayed"
        );
    }

    #[test]
    fn marker_edits_after_claim_are_validated_from_the_pinned_witness_before_deletion() {
        use std::os::unix::fs::MetadataExt;

        let fixture = Fixture::new();
        let (root, mut owner) = fixture.root("root");
        let payload = root.join("retained.txt");
        fs::write(&payload, "must survive changed authority").unwrap();
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let record = inventory(&fixture.database, None).roots.remove(0);
        let fingerprint = ownership_fingerprint(&record).unwrap();
        let mut claim = claim_for_removal(&fixture.database, &root, &fingerprint).unwrap();
        let marker_path = root.join(WORKTREE_ROOT_MARKER);
        let before = fs::metadata(&marker_path).unwrap();
        owner.repository_key = "changed-owner-after-claim".into();
        let changed = serde_json::to_vec(&owner).unwrap();
        fs::write(&marker_path, &changed).unwrap();
        let after = fs::metadata(&marker_path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));

        assert!(claim.remove(None).is_err());
        assert_eq!(
            fs::read_to_string(&payload).unwrap(),
            "must survive changed authority"
        );
        assert_eq!(fs::read(&marker_path).unwrap(), changed);
    }

    #[test]
    fn a_restored_origin_after_claim_revokes_deletion_before_the_first_unlink() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fs::write(
            root.join("retained.txt"),
            "restored repository keeps its worktrees",
        )
        .unwrap();
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let record = inventory(&fixture.database, None).roots.remove(0);
        let fingerprint = ownership_fingerprint(&record).unwrap();
        let mut claim = claim_for_removal(&fixture.database, &root, &fingerprint).unwrap();

        fs::create_dir(&fixture.origin).unwrap();
        assert!(claim.remove(None).is_err());
        assert_eq!(
            fs::read_to_string(root.join("retained.txt")).unwrap(),
            "restored repository keeps its worktrees"
        );
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
    }

    #[test]
    fn readonly_missing_history_does_not_create_state_and_expiry_is_incomplete() {
        let fixture = Fixture::new();
        assert!(!inventory(&fixture.database, None).complete);
        assert!(!fixture.database.parent().unwrap().exists());
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let found = inventory(&fixture.database, Some(Instant::now()));
        assert!(!found.complete);
        assert!(found.roots.is_empty());
    }

    #[test]
    fn changed_root_marker_and_registry_cannot_reuse_a_reviewed_proof() {
        let fixture = Fixture::new();
        let (root, mut owner) = fixture.root("root");
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let record = inventory(&fixture.database, None).roots.remove(0);
        let fingerprint = ownership_fingerprint(&record).unwrap();
        owner.repository_key = "forged-other-owner".into();
        fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();
        assert!(revalidate(&fixture.database, &root, &fingerprint).is_err());
        assert!(!inventory(&fixture.database, None).complete);
        fs::write(root.join(WORKTREE_ROOT_MARKER), b"{}").unwrap();
        assert!(revalidate(&fixture.database, &root, &fingerprint).is_err());
        // Replacing the entire directory cannot retain the old object proof.
        fs::rename(&root, root.with_extension("retained")).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();
        assert!(!inventory(&fixture.database, None).complete);
        assert!(root.with_extension("retained").is_dir());
    }

    #[test]
    fn unavailable_or_replaced_origin_filesystem_is_not_an_orphan() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let mut record = inventory(&fixture.database, None).roots.remove(0);
        assert!(origin_is_missing(&record).unwrap());
        record.origin_identity.as_mut().unwrap().filesystem[0] ^= 1;
        assert!(origin_is_missing(&record).is_err());
        assert!(ownership_fingerprint(&record).is_err());
        record.origin_identity = None;
        assert!(origin_is_missing(&record).is_err());
    }

    #[test]
    fn unverified_marker_symlink_and_oversized_record_are_refused() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fs::rename(root.join(WORKTREE_ROOT_MARKER), root.join("held-marker")).unwrap();
        symlink(root.join("held-marker"), root.join(WORKTREE_ROOT_MARKER)).unwrap();
        assert!(
            register(
                &fixture.database,
                &root,
                &owner,
                WorktreeRootSource::HostState
            )
            .is_err()
        );
        assert!(!registry_path(&fixture.database).unwrap().exists());
        fs::remove_file(root.join(WORKTREE_ROOT_MARKER)).unwrap();
        fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            vec![b'x'; MAX_RECORD_BYTES + 1],
        )
        .unwrap();
        assert!(read_marker(&root).is_err());
        fs::write(
            root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();
        fixture.register(&root, &owner);
        let connection = open_registry(&fixture.database, true).unwrap();
        connection
            .execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        connection
            .execute(
                "UPDATE roots SET payload=?1",
                ["x".repeat(MAX_RECORD_BYTES + 1)],
            )
            .unwrap();
        drop(connection);
        assert!(!inventory(&fixture.database, None).complete);
    }

    #[test]
    fn missing_roots_are_pruned_with_a_rotating_bounded_cursor() {
        let fixture = Fixture::new();
        let mut roots = Vec::new();
        for index in 0..PAGE_RECORDS + 5 {
            let (root, owner) = fixture.root(&format!("root-{index}"));
            fixture.register(&root, &owner);
            roots.push(root);
        }
        for root in &roots {
            fs::remove_dir_all(root).unwrap();
        }
        let (live, owner) = fixture.root("live");
        for _ in 0..4 {
            fixture.register(&live, &owner);
        }
        let connection = open_registry(&fixture.database, false).unwrap();
        let count: usize = connection
            .query_row("SELECT count(*) FROM roots", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "missing metadata must not accumulate forever");
        drop(connection);
        let found = inventory(&fixture.database, None);
        assert!(found.complete, "{:?}", found.issues);
        assert_eq!(found.roots.len(), 1);
    }

    #[test]
    fn symlinked_registry_and_unsupported_schema_are_retained_unknown() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let path = registry_path(&fixture.database).unwrap();
        let kept = path.with_extension("kept");
        fs::rename(&path, &kept).unwrap();
        symlink(&kept, &path).unwrap();
        assert!(!inventory(&fixture.database, None).complete);
        assert!(
            register(
                &fixture.database,
                &root,
                &owner,
                WorktreeRootSource::HostState
            )
            .is_err()
        );
        fs::remove_file(&path).unwrap();
        fs::rename(&kept, &path).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
        drop(connection);
        assert!(!inventory(&fixture.database, None).complete);
    }
    #[test]
    fn full_registry_advances_maintenance_even_when_admission_is_refused() {
        let fixture = Fixture::new();
        let mut roots = Vec::new();
        for index in 0..PAGE_RECORDS + 1 {
            let (root, owner) = fixture.root(&format!("kept-{index}"));
            register_with_limit(
                &fixture.database,
                &root,
                &owner,
                WorktreeRootSource::HostState,
                PAGE_RECORDS + 1,
            )
            .unwrap();
            roots.push(root);
        }
        roots.sort_by_key(|root| record_key(root));
        fs::remove_dir_all(roots.last().unwrap()).unwrap();
        let connection = open_registry(&fixture.database, true).unwrap();
        connection
            .execute(
                "UPDATE registry_meta SET value='' WHERE key='prune_cursor'",
                [],
            )
            .unwrap();
        drop(connection);
        let (new_root, owner) = fixture.root("new-admission");
        assert!(
            register_with_limit(
                &fixture.database,
                &new_root,
                &owner,
                WorktreeRootSource::HostState,
                PAGE_RECORDS + 1
            )
            .is_err()
        );
        let connection = open_registry(&fixture.database, false).unwrap();
        let present: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM roots WHERE key=?1)",
                [record_key(&new_root)],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !present,
            "denied admission must not be registered prematurely"
        );
        drop(connection);
        register_with_limit(
            &fixture.database,
            &new_root,
            &owner,
            WorktreeRootSource::HostState,
            PAGE_RECORDS + 1,
        )
        .unwrap();
        assert!(
            inventory(&fixture.database, None)
                .roots
                .iter()
                .any(|record| record.root == new_root)
        );
    }

    #[test]
    #[ignore = "owned subprocess entrypoint for cross-process registry exclusion"]
    fn registry_admission_child() {
        let Some(database) = std::env::var_os("AETHYME_REGISTRY_TEST_DATABASE") else {
            return;
        };
        let root = PathBuf::from(std::env::var_os("AETHYME_REGISTRY_TEST_ROOT").unwrap());
        let marker = read_marker(&root).unwrap();
        let result = register(
            Path::new(&database),
            &root,
            &marker,
            WorktreeRootSource::HostState,
        );
        let expected_blocked = std::env::var_os("AETHYME_REGISTRY_TEST_BLOCKED").is_some();
        if expected_blocked {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WouldBlock);
        } else {
            result.unwrap();
        }
    }

    #[test]
    fn removal_claim_excludes_new_admission_in_another_process_until_it_drops() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let record = inventory(&fixture.database, None).roots.remove(0);
        let fingerprint = ownership_fingerprint(&record).unwrap();
        let claim = claim_for_removal(&fixture.database, &root, &fingerprint).unwrap();
        // Reappearance would otherwise permit a valid admission. Pause after
        // GC's proof check and let another process attempt precisely that.
        fs::create_dir(&fixture.origin).unwrap();
        let child = |blocked| {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "worktree_registry::tests::registry_admission_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("AETHYME_REGISTRY_TEST_DATABASE", &fixture.database)
                .env("AETHYME_REGISTRY_TEST_ROOT", &root)
                .env_remove("AETHYME_REGISTRY_TEST_BLOCKED");
            if blocked {
                command.env("AETHYME_REGISTRY_TEST_BLOCKED", "1");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "child must actually execute its assertion"
            );
        };
        child(true);
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
        drop(claim);
        child(false);
        assert!(root.is_dir());
    }

    #[test]
    fn registry_never_changes_arbitrary_host_database_parent_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let parent = fixture.database.parent().unwrap();
        fs::create_dir(parent).unwrap();
        fs::set_permissions(parent, fs::Permissions::from_mode(0o755)).unwrap();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        assert_eq!(
            fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(registry_directory(&fixture.database).unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn symlinked_and_hardlinked_registry_locks_cannot_split_exclusion() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        let path = registry_path(&fixture.database)
            .unwrap()
            .with_extension("lock");
        let kept = path.with_extension("kept-lock");
        fs::rename(&path, &kept).unwrap();
        symlink(&kept, &path).unwrap();
        assert!(registry_lock(&fixture.database, true).is_err());
        fs::remove_file(&path).unwrap();
        fs::hard_link(&kept, &path).unwrap();
        assert!(registry_lock(&fixture.database, true).is_err());
        assert_eq!(fs::metadata(&kept).unwrap().len(), 0);
    }
    #[test]
    fn absent_root_requires_original_mounted_filesystem_before_retiring_work() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        fixture.register(&root, &owner);
        fs::remove_dir(&fixture.origin).unwrap();
        let record = inventory(&fixture.database, None).roots.remove(0);
        let fingerprint = ownership_fingerprint(&record).unwrap();
        fs::remove_dir_all(&root).unwrap();
        let claim = claim_for_removal(&fixture.database, &root, &fingerprint).unwrap();
        assert!(claim.already_absent);
        drop(claim);
        let mut changed_volume = record;
        changed_volume.identity.as_mut().unwrap().filesystem[0] ^= 1;
        let payload = serde_json::to_string(&changed_volume).unwrap();
        let fingerprint = format!("{:x}", Sha256::digest(payload.as_bytes()));
        let connection = open_registry(&fixture.database, true).unwrap();
        connection
            .execute(
                "UPDATE roots SET payload=?1 WHERE key=?2",
                params![payload, record_key(&root)],
            )
            .unwrap();
        drop(connection);
        assert!(matches!(
            claim_for_removal(&fixture.database, &root, &fingerprint),
            Err(ClaimError::Unavailable(_))
        ));
    }
    #[test]
    fn registry_cannot_be_placed_inside_a_root_it_may_remove() {
        let fixture = Fixture::new();
        let (root, owner) = fixture.root("root");
        let database = root.join("host-operations.db");
        assert!(
            register(
                &database,
                &root,
                &owner,
                WorktreeRootSource::LibraryOverride
            )
            .is_err()
        );
        assert!(!registry_directory(&database).unwrap().exists());
        assert!(root.join(WORKTREE_ROOT_MARKER).is_file());
    }
}
