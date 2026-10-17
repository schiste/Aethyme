//! Bounded host-state journal for shared preparation-cache rotation debt.
//!
//! The journal lives beside the captured host-resource database, outside the
//! disposable cache tree. Its row makes a rotation discoverable if the cache
//! filesystem disappears or is replaced after publication. The in-cache
//! witness remains the authority for deleting the retired tree.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs::OpenOptions;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

const SCHEMA_VERSION: i64 = 2;
const MAX_RECORD_BYTES: usize = 8 * 1024;
const MAX_ROOT_BYTES: usize = 4 * 1024;
const MAX_ROOT_IDENTITY_BYTES: usize = 512;
const MAX_DEBT_ROWS: i64 = 128;
const MAX_ROOT_ROWS: i64 = 32;
const MAX_PAGE_ROWS: i64 = 32;
const MAX_PAGE_COUNT: i64 = 512;
const MAX_DATABASE_BYTES: u64 = 2 * 1024 * 1024;
const BUSY_TIMEOUT: Duration = Duration::from_millis(150);
const LAP_IDLE: i64 = 0;
const LAP_ACTIVE: i64 = 1;
const LAP_EXHAUSTED: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JournalDebt {
    /// `None` means the stored repository key is malformed. Its bounded raw
    /// sort key is still returned so one corrupt row cannot starve later rows.
    pub(crate) repository: Option<String>,
    pub(crate) sort_key: Vec<u8>,
    pub(crate) payload_valid: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JournalPage {
    pub(crate) generation: u64,
    pub(crate) entries: Vec<JournalDebt>,
    /// The keyset reached its current end. Completion still requires an
    /// explicit acknowledgement after the caller's full inventory succeeds.
    pub(crate) lap_exhausted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JournalProbe {
    NoDebt,
    Pending,
    Unknown,
}

/// Derive a private, stable journal path from the exact captured resource DB
/// path. The hash avoids disclosing host paths in filenames while keeping
/// distinct captured databases in separate recovery domains.
pub(crate) fn path_for_resource_db(resource_db: &Path) -> io::Result<PathBuf> {
    let bytes = resource_db.as_os_str().as_bytes();
    if !resource_db.is_absolute() || bytes.is_empty() || bytes.len() > MAX_ROOT_BYTES {
        return Err(invalid_input("captured host resource DB path is invalid"));
    }
    let parent = resource_db
        .parent()
        .ok_or_else(|| invalid_input("captured host resource DB has no parent"))?;
    Ok(parent.join(format!(
        ".aethyme-preparation-rotation-{:x}.sqlite3",
        Sha256::digest(bytes)
    )))
}

/// Publish the exact debt bytes and bump the durable generation in one FULL
/// synchronous SQLite transaction. This must finish before the in-cache
/// witness is written or the original store is renamed.
pub(crate) fn publish(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
    repository: &str,
    payload: &[u8],
) -> io::Result<()> {
    validate_inputs(cache_root, root_identity, repository, payload)?;
    let path = path_for_resource_db(resource_db)?;
    let mut connection = open_writer(&path)?;
    ensure_writer_schema(&connection)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let root = cache_root.as_os_str().as_bytes();
    let generation = ensure_root(&transaction, root, root_identity)?;
    let existing: Option<Vec<u8>> = transaction
        .query_row(
            "SELECT payload FROM rotation_debt WHERE root_path=?1 AND repository=?2",
            params![root, repository],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some(existing) = existing {
        if existing != payload {
            return Err(invalid_data(
                "a different rotation debt is already journaled for this repository",
            ));
        }
        transaction.commit().map_err(sqlite_error)?;
        return check_database_size(&path);
    }
    let count: i64 = transaction
        .query_row("SELECT count(*) FROM rotation_debt", [], |row| row.get(0))
        .map_err(sqlite_error)?;
    if count >= MAX_DEBT_ROWS {
        return Err(invalid_data("rotation debt journal row cap is reached"));
    }
    let next_generation = generation
        .checked_add(1)
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or_else(|| invalid_data("rotation debt generation is exhausted"))?;
    transaction
        .execute(
            "INSERT INTO rotation_debt(root_path, repository, payload) VALUES (?1, ?2, ?3)",
            params![root, repository, payload],
        )
        .map_err(sqlite_error)?;
    transaction
        .execute(
            "UPDATE rotation_root SET generation=?2 WHERE root_path=?1",
            params![root, next_generation as i64],
        )
        .map_err(sqlite_error)?;
    transaction.commit().map_err(sqlite_error)?;
    check_database_size(&path)
}

/// Read one exact row while checking that the journal's remembered cache-root
/// identity still matches this invocation. A replaced or unavailable root is
/// an error and cannot be used to reconstruct an in-cache witness.
pub(crate) fn load(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
    repository: &str,
) -> io::Result<Option<Vec<u8>>> {
    validate_root(cache_root, root_identity)?;
    validate_repository(repository)?;
    let path = path_for_resource_db(resource_db)?;
    let Some(connection) = open_reader(&path)? else {
        return Ok(None);
    };
    if !ensure_schema(&connection, false)? {
        return Err(invalid_data("rotation debt journal schema is absent"));
    }
    let root = cache_root.as_os_str().as_bytes();
    let expected: Option<Vec<u8>> = connection
        .query_row(
            "SELECT root_identity FROM rotation_root WHERE root_path=?1",
            params![root],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(expected) = expected else {
        return Ok(None);
    };
    if expected != root_identity {
        return Err(invalid_data(
            "preparation cache root identity differs from its durable journal",
        ));
    }
    let payload: Option<Vec<u8>> = connection
        .query_row(
            "SELECT length(payload) FROM rotation_debt WHERE root_path=?1 AND repository=?2",
            params![root, repository],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(length) = payload else {
        return Ok(None);
    };
    if !(1..=MAX_RECORD_BYTES as i64).contains(&length) {
        return Err(invalid_data("rotation debt row exceeds its size limit"));
    }
    connection
        .query_row(
            "SELECT payload FROM rotation_debt WHERE root_path=?1 AND repository=?2",
            params![root, repository],
            |row| row.get(0),
        )
        .map(Some)
        .map_err(sqlite_error)
}

/// Advance the durable keyset cursor before the caller acquires any
/// per-repository lease. The SQLite transaction is closed before filesystem
/// recovery, so publisher lock order remains repository lease -> journal.
///
/// Call this only after a new full cache-root inventory succeeded. An active
/// lap resumes its persisted cursor and generation unchanged; an idle or
/// exhausted lap starts at the current generation. Starting a lap never moves
/// `completed_generation`, so pending work remains visible until acknowledged.
pub(crate) fn begin_lap(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
) -> io::Result<u64> {
    validate_root(cache_root, root_identity)?;
    let path = path_for_resource_db(resource_db)?;
    let mut connection = open_writer(&path)?;
    ensure_writer_schema(&connection)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let root = cache_root.as_os_str().as_bytes();
    let generation = ensure_root(&transaction, root, root_identity)?;
    let lap_state: i64 = transaction
        .query_row(
            "SELECT lap_state FROM rotation_root WHERE root_path=?1",
            params![root],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    validate_lap_state(lap_state)?;
    if lap_state != LAP_ACTIVE {
        transaction
            .execute(
                "UPDATE rotation_root SET lap_generation=?2, cursor=NULL, \
                    lap_incomplete=0, lap_state=?3 WHERE root_path=?1",
                params![root, generation as i64, LAP_ACTIVE],
            )
            .map_err(sqlite_error)?;
    }
    transaction.commit().map_err(sqlite_error)?;
    check_database_size(&path)?;
    Ok(generation)
}

pub(crate) fn next_page(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
) -> io::Result<JournalPage> {
    next_page_inner(resource_db, cache_root, root_identity, MAX_PAGE_ROWS)
}

#[cfg(test)]
fn next_page_with_limit(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
    page_rows: i64,
) -> io::Result<JournalPage> {
    next_page_inner(resource_db, cache_root, root_identity, page_rows)
}

fn next_page_inner(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
    page_rows: i64,
) -> io::Result<JournalPage> {
    if !(1..=MAX_PAGE_ROWS).contains(&page_rows) {
        return Err(invalid_input("rotation journal page size is invalid"));
    }
    validate_root(cache_root, root_identity)?;
    let path = path_for_resource_db(resource_db)?;
    let mut connection = open_writer(&path)?;
    ensure_writer_schema(&connection)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let root = cache_root.as_os_str().as_bytes();
    let generation = ensure_root(&transaction, root, root_identity)?;
    let (lap_generation, mut cursor, lap_state): (i64, Option<Vec<u8>>, i64) = transaction
        .query_row(
            "SELECT lap_generation, cursor, lap_state \
             FROM rotation_root WHERE root_path=?1",
            params![root],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(sqlite_error)?;
    validate_lap_state(lap_state)?;
    if lap_state != LAP_ACTIVE {
        return Err(invalid_data(
            "rotation journal lap must be started before reading pages",
        ));
    }
    if generation as i64 != lap_generation && cursor.is_none() {
        transaction
            .execute(
                "UPDATE rotation_root SET lap_generation=?2 \
                 WHERE root_path=?1",
                params![root, generation as i64],
            )
            .map_err(sqlite_error)?;
    } else if generation as i64 != lap_generation {
        // Keep the in-flight keyset cursor so repeated low-key publications
        // cannot restart the same prefix forever. This lap cannot complete
        // because it did not observe a stable generation.
        transaction
            .execute(
                "UPDATE rotation_root SET lap_incomplete=1 WHERE root_path=?1",
                params![root],
            )
            .map_err(sqlite_error)?;
    }

    let mut entries = Vec::new();
    {
        let mut statement = transaction
            .prepare(
                "SELECT CAST(repository AS BLOB), length(payload) FROM rotation_debt \
                 WHERE root_path=?1 AND (?2 IS NULL OR CAST(repository AS BLOB)>?2) \
                 ORDER BY CAST(repository AS BLOB) LIMIT ?3",
            )
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(params![root, cursor, page_rows], |row| {
                let sort_key: Vec<u8> = row.get(0)?;
                let payload_length: i64 = row.get(1)?;
                let repository = std::str::from_utf8(&sort_key)
                    .ok()
                    .filter(|value| valid_repository(value))
                    .map(str::to_owned);
                Ok(JournalDebt {
                    repository,
                    sort_key,
                    payload_valid: (1..=MAX_RECORD_BYTES as i64).contains(&payload_length),
                })
            })
            .map_err(sqlite_error)?;
        for row in rows {
            let row = row.map_err(sqlite_error)?;
            if row.sort_key.len() > MAX_DATABASE_BYTES as usize {
                return Err(invalid_data("rotation debt key exceeds the database bound"));
            }
            entries.push(row);
        }
    }
    if let Some(last) = entries.last() {
        transaction
            .execute(
                "UPDATE rotation_root SET cursor=?2 WHERE root_path=?1",
                params![root, last.sort_key],
            )
            .map_err(sqlite_error)?;
        transaction.commit().map_err(sqlite_error)?;
        check_database_size(&path)?;
        return Ok(JournalPage {
            generation,
            entries,
            lap_exhausted: false,
        });
    }

    transaction
        .execute(
            "UPDATE rotation_root SET cursor=NULL, lap_state=?2 WHERE root_path=?1",
            params![root, LAP_EXHAUSTED],
        )
        .map_err(sqlite_error)?;
    transaction.commit().map_err(sqlite_error)?;
    check_database_size(&path)?;
    Ok(JournalPage {
        generation,
        entries,
        lap_exhausted: true,
    })
}

/// Acknowledge a complete keyset lap only after the caller's full cache-root
/// inventory and all debt work succeeded. Rechecking the generation, end-of-
/// lap state and empty debt set under one transaction prevents an external
/// publication or unresolved row from being reported as complete.
pub(crate) fn acknowledge_lap(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
    expected_generation: u64,
) -> io::Result<()> {
    validate_root(cache_root, root_identity)?;
    let expected_generation = i64::try_from(expected_generation)
        .map_err(|_| invalid_input("rotation generation is out of range"))?;
    let path = path_for_resource_db(resource_db)?;
    let mut connection = open_writer(&path)?;
    ensure_writer_schema(&connection)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let root = cache_root.as_os_str().as_bytes();
    ensure_root(&transaction, root, root_identity)?;
    let (generation, completed, lap_generation, cursor, incomplete, lap_state): (
        i64,
        i64,
        i64,
        Option<Vec<u8>>,
        i64,
        i64,
    ) = transaction
        .query_row(
            "SELECT generation, completed_generation, lap_generation, cursor, \
                lap_incomplete, lap_state FROM rotation_root WHERE root_path=?1",
            params![root],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .map_err(sqlite_error)?;
    validate_lap_state(lap_state)?;
    validate_rotation_state(
        generation,
        completed,
        lap_generation,
        cursor.as_deref(),
        incomplete,
        lap_state,
    )?;
    let debt_count: i64 = transaction
        .query_row(
            "SELECT count(*) FROM rotation_debt WHERE root_path=?1",
            params![root],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    if generation != expected_generation
        || lap_generation != generation
        || incomplete != 0
        || lap_state != LAP_EXHAUSTED
        || cursor.is_some()
        || debt_count != 0
        || completed > generation
    {
        return Err(invalid_data(
            "rotation journal lap is not eligible for completion",
        ));
    }
    transaction
        .execute(
            "UPDATE rotation_root SET completed_generation=?2, lap_state=?3 \
             WHERE root_path=?1",
            params![root, generation, LAP_IDLE],
        )
        .map_err(sqlite_error)?;
    transaction.commit().map_err(sqlite_error)?;
    check_database_size(&path)
}

/// Keep a lap incomplete when filesystem or lease state could not be
/// classified. Rows themselves also remain pending; this bit covers failures
/// discovered outside a specific journal row, such as an incomplete bounded
/// cache-root inventory.
pub(crate) fn mark_lap_incomplete(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
) -> io::Result<()> {
    validate_root(cache_root, root_identity)?;
    let path = path_for_resource_db(resource_db)?;
    let mut connection = open_writer(&path)?;
    ensure_writer_schema(&connection)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let root = cache_root.as_os_str().as_bytes();
    ensure_root(&transaction, root, root_identity)?;
    transaction
        .execute(
            "UPDATE rotation_root SET lap_incomplete=1 WHERE root_path=?1",
            params![root],
        )
        .map_err(sqlite_error)?;
    transaction.commit().map_err(sqlite_error)?;
    check_database_size(&path)
}

/// Remove a row only after the caller has completed the pinned filesystem
/// transition and removed the in-cache witness. If a crash occurs first, the
/// external row remains discoverable and the next pass revalidates it.
pub(crate) fn complete(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
    repository: &str,
    expected_payload: &[u8],
) -> io::Result<()> {
    validate_inputs(cache_root, root_identity, repository, expected_payload)?;
    let path = path_for_resource_db(resource_db)?;
    let mut connection = open_writer(&path)?;
    ensure_writer_schema(&connection)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let root = cache_root.as_os_str().as_bytes();
    ensure_root(&transaction, root, root_identity)?;
    let current: Option<Vec<u8>> = transaction
        .query_row(
            "SELECT payload FROM rotation_debt WHERE root_path=?1 AND repository=?2",
            params![root, repository],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    match current {
        Some(current) if current == expected_payload => {
            transaction
                .execute(
                    "DELETE FROM rotation_debt WHERE root_path=?1 AND repository=?2",
                    params![root, repository],
                )
                .map_err(sqlite_error)?;
        }
        None => {}
        Some(_) => return Err(invalid_data("rotation debt row changed before completion")),
    }
    transaction.commit().map_err(sqlite_error)?;
    check_database_size(&path)
}

/// Read the journal without creating or mutating it. `Ok(None)` means there
/// is no journal file yet; callers must still inspect bounded in-cache
/// witnesses for legacy or interrupted publication states.
pub(crate) fn probe(
    resource_db: &Path,
    cache_root: &Path,
    current_root_identity: Option<&[u8]>,
) -> io::Result<Option<JournalProbe>> {
    let path = path_for_resource_db(resource_db)?;
    let Some(connection) = open_reader(&path)? else {
        return Ok(None);
    };
    if !ensure_schema(&connection, false)? {
        return Err(invalid_data("rotation debt journal schema is absent"));
    }
    let root = root_path_bytes(cache_root)?;
    let state: Option<(Vec<u8>, i64, i64, i64, Option<Vec<u8>>, i64, i64)> = connection
        .query_row(
            "SELECT root_identity, generation, completed_generation, lap_generation, cursor, \
                lap_incomplete, lap_state \
             FROM rotation_root WHERE root_path=?1",
            params![root],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((
        expected_identity,
        generation,
        completed_generation,
        lap_generation,
        cursor,
        incomplete,
        lap_state,
    )) = state
    else {
        return Ok(Some(JournalProbe::NoDebt));
    };
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM rotation_debt WHERE root_path=?1",
            params![root],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    if count > MAX_DEBT_ROWS {
        return Err(invalid_data("rotation journal state is malformed"));
    }
    validate_rotation_state(
        generation,
        completed_generation,
        lap_generation,
        cursor.as_deref(),
        incomplete,
        lap_state,
    )?;
    let pending = count > 0
        || generation > completed_generation
        || lap_generation != generation
        || incomplete != 0
        || lap_state != LAP_IDLE;
    if let Some(actual) = current_root_identity {
        if actual != expected_identity.as_slice() {
            return Ok(Some(if pending {
                JournalProbe::Unknown
            } else {
                JournalProbe::NoDebt
            }));
        }
    } else if pending {
        return Ok(Some(JournalProbe::Unknown));
    }
    Ok(Some(if pending {
        JournalProbe::Pending
    } else {
        JournalProbe::NoDebt
    }))
}

fn validate_inputs(
    cache_root: &Path,
    root_identity: &[u8],
    repository: &str,
    payload: &[u8],
) -> io::Result<()> {
    validate_root(cache_root, root_identity)?;
    validate_repository(repository)?;
    if payload.is_empty() || payload.len() > MAX_RECORD_BYTES {
        return Err(invalid_input("rotation debt payload has invalid size"));
    }
    Ok(())
}

fn validate_root(cache_root: &Path, root_identity: &[u8]) -> io::Result<()> {
    root_path_bytes(cache_root)?;
    if root_identity.is_empty() || root_identity.len() > MAX_ROOT_IDENTITY_BYTES {
        return Err(invalid_input(
            "preparation cache root identity has invalid size",
        ));
    }
    Ok(())
}

fn root_path_bytes(path: &Path) -> io::Result<&[u8]> {
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute() || bytes.is_empty() || bytes.len() > MAX_ROOT_BYTES {
        return Err(invalid_input("preparation cache root path is invalid"));
    }
    Ok(bytes)
}

fn validate_repository(repository: &str) -> io::Result<()> {
    if valid_repository(repository) {
        Ok(())
    } else {
        Err(invalid_input("rotation repository key is malformed"))
    }
}

fn valid_repository(repository: &str) -> bool {
    repository.len() == 64
        && repository
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn open_writer(path: &Path) -> io::Result<Connection> {
    validate_parent(path)?;
    validate_existing_file(path)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection = Connection::open_with_flags(path, flags).map_err(sqlite_error)?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(sqlite_error)?;
    tighten_database_permissions(path)?;
    connection
        .pragma_update(None, "page_size", 4096_i64)
        .map_err(sqlite_error)?;
    let page_size: i64 = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .map_err(sqlite_error)?;
    if page_size != 4096 {
        return Err(invalid_data("rotation journal page size is not bounded"));
    }
    connection
        .pragma_update(None, "journal_mode", "DELETE")
        .map_err(sqlite_error)?;
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(sqlite_error)?;
    if !journal_mode.eq_ignore_ascii_case("delete") {
        return Err(invalid_data("rotation journal is not in DELETE mode"));
    }
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "trusted_schema", "OFF")
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "journal_size_limit", 0_i64)
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "max_page_count", MAX_PAGE_COUNT)
        .map_err(sqlite_error)?;
    Ok(connection)
}

fn open_reader(path: &Path) -> io::Result<Option<Connection>> {
    validate_parent(path)?;
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    validate_existing_file(path)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection = Connection::open_with_flags(path, flags).map_err(sqlite_error)?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(sqlite_error)?;
    connection
        .pragma_update(None, "query_only", "ON")
        .map_err(sqlite_error)?;
    Ok(Some(connection))
}

fn validate_parent(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_input("rotation journal has no parent"))?;
    let metadata = std::fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != effective_uid()
        || metadata.mode() & 0o022 != 0
    {
        return Err(invalid_data(
            "rotation journal parent is not private and real",
        ));
    }
    Ok(())
}

fn validate_existing_file(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(metadata) => {
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.nlink() != 1
                || metadata.uid() != effective_uid()
                || metadata.mode() & 0o077 != 0
                || metadata.len() > MAX_DATABASE_BYTES
            {
                return Err(invalid_data("rotation journal file identity is unsafe"));
            }
            Ok(())
        }
    }
}

fn tighten_database_permissions(path: &Path) -> io::Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != effective_uid()
        || metadata.nlink() != 1
        || metadata.len() > MAX_DATABASE_BYTES
    {
        return Err(invalid_data("rotation journal file identity is unsafe"));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn effective_uid() -> u32 {
    // SAFETY: `geteuid` has no pointer arguments and returns the caller's effective UID.
    unsafe { libc::geteuid() }
}

fn ensure_schema(connection: &Connection, create: bool) -> io::Result<bool> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(sqlite_error)?;
    if version == SCHEMA_VERSION {
        let roots: bool = connection
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='rotation_root'",
                [],
                |_| Ok(true),
            )
            .optional()
            .map_err(sqlite_error)?
            .unwrap_or(false);
        let debts: bool = connection
            .query_row(
                "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='rotation_debt'",
                [],
                |_| Ok(true),
            )
            .optional()
            .map_err(sqlite_error)?
            .unwrap_or(false);
        if !roots || !debts {
            return Err(invalid_data("rotation journal schema is incomplete"));
        }
        connection
            .prepare("SELECT lap_state FROM rotation_root LIMIT 0")
            .map_err(sqlite_error)?;
        return Ok(true);
    }
    if version != 0 || !create {
        return Ok(false);
    }
    let tables: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    if tables != 0 {
        return Err(invalid_data("unversioned rotation journal is not empty"));
    }
    let transaction = connection.unchecked_transaction().map_err(sqlite_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE rotation_root(
                root_path BLOB PRIMARY KEY NOT NULL CHECK(length(root_path) BETWEEN 1 AND 4096),
                root_identity BLOB NOT NULL CHECK(length(root_identity) BETWEEN 1 AND 512),
                generation INTEGER NOT NULL CHECK(generation >= 0),
                completed_generation INTEGER NOT NULL CHECK(completed_generation >= 0 AND completed_generation <= generation),
                lap_generation INTEGER NOT NULL CHECK(lap_generation >= 0 AND lap_generation <= generation),
                cursor BLOB,
                lap_incomplete INTEGER NOT NULL CHECK(lap_incomplete IN (0,1)),
                lap_state INTEGER NOT NULL CHECK(lap_state IN (0,1,2))
             ) WITHOUT ROWID;
             CREATE TABLE rotation_debt(
                root_path BLOB NOT NULL REFERENCES rotation_root(root_path) ON DELETE CASCADE,
                repository TEXT NOT NULL CHECK(length(repository)=64),
                payload BLOB NOT NULL CHECK(length(payload) BETWEEN 1 AND 8192),
                PRIMARY KEY(root_path, repository)
             ) WITHOUT ROWID;
             PRAGMA user_version=2;",
        )
        .map_err(sqlite_error)?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(true)
}

fn ensure_writer_schema(connection: &Connection) -> io::Result<()> {
    if ensure_schema(connection, true)? {
        Ok(())
    } else {
        Err(invalid_data(
            "rotation journal schema version is unsupported",
        ))
    }
}

fn ensure_root(
    transaction: &rusqlite::Transaction<'_>,
    root: &[u8],
    root_identity: &[u8],
) -> io::Result<u64> {
    let existing: Option<(Vec<u8>, i64, i64, i64, Option<Vec<u8>>, i64, i64)> = transaction
        .query_row(
            "SELECT root_identity, generation, completed_generation, lap_generation, cursor, lap_incomplete, lap_state \
             FROM rotation_root WHERE root_path=?1",
            params![root],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    match existing {
        Some((identity, generation, completed, lap_generation, cursor, incomplete, lap_state))
            if identity == root_identity =>
        {
            validate_rotation_state(
                generation,
                completed,
                lap_generation,
                cursor.as_deref(),
                incomplete,
                lap_state,
            )?;
            u64::try_from(generation).map_err(|_| invalid_data("negative rotation generation"))
        }
        Some((_identity, generation, completed, lap_generation, cursor, incomplete, lap_state)) => {
            validate_rotation_state(
                generation,
                completed,
                lap_generation,
                cursor.as_deref(),
                incomplete,
                lap_state,
            )?;
            let count: i64 = transaction
                .query_row(
                    "SELECT count(*) FROM rotation_debt WHERE root_path=?1",
                    params![root],
                    |row| row.get(0),
                )
                .map_err(sqlite_error)?;
            if count != 0
                || generation != completed
                || lap_generation != generation
                || cursor.is_some()
                || incomplete != 0
                || lap_state != LAP_IDLE
            {
                return Err(invalid_data(
                    "preparation cache root changed with rotation debt pending",
                ));
            }
            transaction
                .execute(
                    "UPDATE rotation_root SET root_identity=?2, generation=0, \
                     completed_generation=0, lap_generation=0, cursor=NULL, lap_incomplete=0, lap_state=?3 \
                     WHERE root_path=?1",
                    params![root, root_identity, LAP_IDLE],
                )
                .map_err(sqlite_error)?;
            Ok(0)
        }
        None => {
            let roots: i64 = transaction
                .query_row("SELECT count(*) FROM rotation_root", [], |row| row.get(0))
                .map_err(sqlite_error)?;
            if roots >= MAX_ROOT_ROWS {
                return Err(invalid_data("rotation journal root cap is reached"));
            }
            transaction
                .execute(
                    "INSERT INTO rotation_root(root_path, root_identity, generation, \
                     completed_generation, lap_generation, cursor, lap_incomplete, lap_state) \
                     VALUES (?1, ?2, 0, 0, 0, NULL, 0, ?3)",
                    params![root, root_identity, LAP_IDLE],
                )
                .map_err(sqlite_error)?;
            Ok(0)
        }
    }
}

fn validate_lap_state(state: i64) -> io::Result<()> {
    if matches!(state, LAP_IDLE | LAP_ACTIVE | LAP_EXHAUSTED) {
        Ok(())
    } else {
        Err(invalid_data("rotation journal lap state is malformed"))
    }
}

fn validate_rotation_state(
    generation: i64,
    completed: i64,
    lap_generation: i64,
    cursor: Option<&[u8]>,
    incomplete: i64,
    lap_state: i64,
) -> io::Result<()> {
    validate_lap_state(lap_state)?;
    if generation < 0
        || completed < 0
        || completed > generation
        || lap_generation < 0
        || lap_generation > generation
        || !matches!(incomplete, 0 | 1)
        || (lap_state != LAP_ACTIVE && cursor.is_some())
    {
        return Err(invalid_data("rotation journal state is malformed"));
    }
    Ok(())
}

fn check_database_size(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.len() > MAX_DATABASE_BYTES
    {
        return Err(invalid_data("rotation journal exceeded its file bound"));
    }
    Ok(())
}

fn sqlite_error(error: rusqlite::Error) -> io::Error {
    let kind = match &error {
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::DatabaseBusy
                || failure.code == rusqlite::ErrorCode::DatabaseLocked =>
        {
            io::ErrorKind::WouldBlock
        }
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::DiskFull =>
        {
            io::ErrorKind::StorageFull
        }
        _ => io::ErrorKind::InvalidData,
    };
    io::Error::new(kind, format!("rotation debt journal: {error}"))
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_owned())
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const ROOT_IDENTITY: &[u8] = b"root-identity-one";
    const REPOSITORY_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const REPOSITORY_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const REPOSITORY_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const REPOSITORY_D: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    struct Fixture {
        _directory: TempDir,
        resource_db: PathBuf,
        cache_root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let resource_db = directory.path().join("host-resources.db");
            let cache_root = directory.path().join("preparation-cache");
            std::fs::create_dir(&cache_root).unwrap();
            Self {
                _directory: directory,
                resource_db,
                cache_root,
            }
        }

        fn publish(&self, repository: &str, payload: &[u8]) {
            publish(
                &self.resource_db,
                &self.cache_root,
                ROOT_IDENTITY,
                repository,
                payload,
            )
            .unwrap();
        }
    }

    #[test]
    fn publish_is_atomic_idempotent_and_discoverable_after_reopen() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"debt-a");
        fixture.publish(REPOSITORY_A, b"debt-a");

        assert_eq!(
            probe(
                &fixture.resource_db,
                &fixture.cache_root,
                Some(ROOT_IDENTITY)
            )
            .unwrap(),
            Some(JournalProbe::Pending)
        );
        assert_eq!(
            load(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                REPOSITORY_A
            )
            .unwrap(),
            Some(b"debt-a".to_vec())
        );
        begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        let page = next_page(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        assert_eq!(page.generation, 1);
        assert_eq!(page.entries.len(), 1);
        assert!(!page.lap_exhausted);
    }

    #[test]
    fn cursor_advances_before_recovery_and_rotates_fairly_after_an_incomplete_lap() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"a");
        fixture.publish(REPOSITORY_B, b"b");
        fixture.publish(REPOSITORY_C, b"c");
        begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();

        let first =
            next_page_with_limit(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 2)
                .unwrap();
        assert_eq!(
            first
                .entries
                .iter()
                .map(|entry| entry.repository.as_str())
                .collect::<Vec<_>>(),
            vec![REPOSITORY_A, REPOSITORY_B]
        );
        mark_lap_incomplete(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        let second =
            next_page_with_limit(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 2)
                .unwrap();
        assert_eq!(second.entries[0].repository.as_deref(), Some(REPOSITORY_C));
        assert!(!second.lap_exhausted);
        let wrapped =
            next_page_with_limit(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 2)
                .unwrap();
        assert!(wrapped.entries.is_empty());
        assert!(wrapped.lap_exhausted);
        assert!(
            acknowledge_lap(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                wrapped.generation,
            )
            .is_err()
        );
        begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        let retry =
            next_page_with_limit(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 2)
                .unwrap();
        assert_eq!(retry.entries[0].repository.as_deref(), Some(REPOSITORY_A));
    }

    #[test]
    fn fresh_inventory_resumes_cursor_but_only_success_acknowledges_completion() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"a");
        fixture.publish(REPOSITORY_B, b"b");
        let first_generation =
            begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        let first =
            next_page_with_limit(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 1)
                .unwrap();
        assert_eq!(first.entries[0].repository.as_deref(), Some(REPOSITORY_A));
        mark_lap_incomplete(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();

        let low_key = format!("{:064x}", 0);
        fixture.publish(&low_key, b"new-low-key");
        let resumed_generation =
            begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        assert_eq!(resumed_generation, first_generation + 1);
        let resumed =
            next_page_with_limit(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 1)
                .unwrap();
        assert_eq!(resumed.entries[0].repository.as_deref(), Some(REPOSITORY_B));
        let end = next_page_with_limit(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 1)
            .unwrap();
        assert!(end.lap_exhausted);
        assert!(
            acknowledge_lap(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                resumed_generation,
            )
            .is_err()
        );

        let retry_generation =
            begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        let retry =
            next_page_with_limit(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 1)
                .unwrap();
        assert_eq!(retry.entries[0].repository, Some(low_key));
        complete(
            &fixture.resource_db,
            &fixture.cache_root,
            ROOT_IDENTITY,
            &low_key,
            b"new-low-key",
        )
        .unwrap();
        complete(
            &fixture.resource_db,
            &fixture.cache_root,
            ROOT_IDENTITY,
            REPOSITORY_A,
            b"a",
        )
        .unwrap();
        complete(
            &fixture.resource_db,
            &fixture.cache_root,
            ROOT_IDENTITY,
            REPOSITORY_B,
            b"b",
        )
        .unwrap();
        let end = next_page_with_limit(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 1)
            .unwrap();
        assert!(end.lap_exhausted);
        acknowledge_lap(
            &fixture.resource_db,
            &fixture.cache_root,
            ROOT_IDENTITY,
            retry_generation,
        )
        .unwrap();
        assert_eq!(
            probe(
                &fixture.resource_db,
                &fixture.cache_root,
                Some(ROOT_IDENTITY)
            )
            .unwrap(),
            Some(JournalProbe::NoDebt)
        );
    }

    #[test]
    fn publication_after_exhaustion_prevents_acknowledging_the_old_generation() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"a");
        let first_generation =
            begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        let page = next_page(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        complete(
            &fixture.resource_db,
            &fixture.cache_root,
            ROOT_IDENTITY,
            REPOSITORY_A,
            b"a",
        )
        .unwrap();
        assert!(
            next_page(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY)
                .unwrap()
                .lap_exhausted
        );

        fixture.publish(REPOSITORY_B, b"b");
        assert!(
            acknowledge_lap(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                first_generation,
            )
            .is_err()
        );
        let second_generation =
            begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        assert_eq!(second_generation, first_generation + 1);
        let pending = next_page(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        assert_eq!(pending.entries[0].repository.as_deref(), Some(REPOSITORY_B));
        assert!(!page.lap_exhausted);
    }

    #[test]
    fn repeated_low_key_publications_do_not_starve_later_repositories() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"a");
        fixture.publish(REPOSITORY_B, b"b");
        fixture.publish(REPOSITORY_C, b"c");
        fixture.publish(REPOSITORY_D, b"d");
        begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();

        let first =
            next_page_with_limit(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 1)
                .unwrap();
        assert_eq!(first.entries[0].repository.as_deref(), Some(REPOSITORY_A));

        for (low_key, expected) in [
            (format!("{:064x}", 0), REPOSITORY_B),
            (format!("{:064x}", 1), REPOSITORY_C),
            (format!("{:064x}", 2), REPOSITORY_D),
        ] {
            fixture.publish(&low_key, b"new-low-key");
            begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
            let page =
                next_page_with_limit(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 1)
                    .unwrap();
            assert_eq!(page.entries[0].repository.as_deref(), Some(expected));
            assert!(!page.lap_exhausted);

            let connection =
                Connection::open(path_for_resource_db(&fixture.resource_db).unwrap()).unwrap();
            let (lap_generation, cursor, incomplete): (i64, Vec<u8>, i64) = connection
                .query_row(
                    "SELECT lap_generation, cursor, lap_incomplete FROM rotation_root \
                     WHERE root_path=?1",
                    params![fixture.cache_root.as_os_str().as_bytes()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(lap_generation, 4);
            assert_eq!(cursor, expected.as_bytes());
            assert_eq!(incomplete, 1);
        }

        begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        let end = next_page_with_limit(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 1)
            .unwrap();
        assert!(end.entries.is_empty());
        assert!(end.lap_exhausted);
        assert!(
            acknowledge_lap(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                end.generation,
            )
            .is_err()
        );

        begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        let next_lap =
            next_page_with_limit(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 1)
                .unwrap();
        assert_eq!(next_lap.entries[0].repository, Some(format!("{:064x}", 0)));
    }

    #[test]
    fn writers_refuse_a_compatible_future_schema_without_mutating_rows() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"debt-a");
        let path = path_for_resource_db(&fixture.resource_db).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(connection);

        assert!(
            publish(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                REPOSITORY_B,
                b"debt-b"
            )
            .is_err()
        );
        assert!(begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).is_err());
        assert!(next_page(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).is_err());
        assert!(
            acknowledge_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY, 1,).is_err()
        );
        assert!(
            mark_lap_incomplete(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).is_err()
        );
        assert!(
            complete(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                REPOSITORY_A,
                b"debt-a"
            )
            .is_err()
        );

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let (generation, debt_count, payload): (i64, i64, Vec<u8>) = connection
            .query_row(
                "SELECT generation, \
                    (SELECT count(*) FROM rotation_debt), \
                    (SELECT payload FROM rotation_debt WHERE repository=?1) \
                 FROM rotation_root WHERE root_path=?2",
                params![REPOSITORY_A, fixture.cache_root.as_os_str().as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION + 1);
        assert_eq!(generation, 1);
        assert_eq!(debt_count, 1);
        assert_eq!(payload, b"debt-a");
    }

    #[test]
    fn malformed_repository_rows_advance_the_cursor_without_starving_valid_debt() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"a");
        fixture.publish(REPOSITORY_B, b"b");
        let path = path_for_resource_db(&fixture.resource_db).unwrap();
        let mut connection = open_writer(&path).unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let root = fixture.cache_root.as_os_str().as_bytes();
        transaction
            .execute(
                "INSERT INTO rotation_debt(root_path, repository, payload) VALUES (?1, ?2, ?3)",
                params![root, "!".repeat(64), b"malformed".as_slice()],
            )
            .unwrap();
        transaction
            .execute(
                "UPDATE rotation_root SET generation=generation+1 WHERE root_path=?1",
                params![root],
            )
            .unwrap();
        transaction.commit().unwrap();

        begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        let page = next_page(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        assert_eq!(page.entries[0].repository, None);
        assert!(
            page.entries[1..]
                .iter()
                .any(|entry| entry.repository.as_deref() == Some(REPOSITORY_A))
        );
        assert!(
            page.entries[1..]
                .iter()
                .any(|entry| entry.repository.as_deref() == Some(REPOSITORY_B))
        );
    }

    #[test]
    fn pending_journal_refuses_a_replaced_cache_root_identity() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"debt-a");

        assert_eq!(
            probe(
                &fixture.resource_db,
                &fixture.cache_root,
                Some(b"replacement-root")
            )
            .unwrap(),
            Some(JournalProbe::Unknown)
        );
        assert!(
            load(
                &fixture.resource_db,
                &fixture.cache_root,
                b"replacement-root",
                REPOSITORY_A
            )
            .is_err()
        );
    }

    #[test]
    fn only_exact_completed_payload_can_remove_a_debt_row() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"debt-a");
        assert!(
            complete(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                REPOSITORY_A,
                b"different"
            )
            .is_err()
        );
        let generation =
            begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).unwrap();
        assert!(
            !next_page(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY)
                .unwrap()
                .lap_exhausted
        );
        complete(
            &fixture.resource_db,
            &fixture.cache_root,
            ROOT_IDENTITY,
            REPOSITORY_A,
            b"debt-a",
        )
        .unwrap();
        assert!(
            next_page(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY)
                .unwrap()
                .lap_exhausted
        );
        acknowledge_lap(
            &fixture.resource_db,
            &fixture.cache_root,
            ROOT_IDENTITY,
            generation,
        )
        .unwrap();
        assert_eq!(
            load(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                REPOSITORY_A
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn row_and_database_bounds_fail_before_unbounded_growth() {
        let fixture = Fixture::new();
        for index in 0..MAX_DEBT_ROWS {
            let repository = format!("{index:064x}");
            fixture.publish(&repository, b"bounded");
        }
        assert!(
            publish(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                &format!("{:064x}", MAX_DEBT_ROWS),
                b"one too many"
            )
            .is_err()
        );
        assert!(check_database_size(&path_for_resource_db(&fixture.resource_db).unwrap()).is_ok());
    }
}
