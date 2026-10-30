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

const SCHEMA_VERSION: i64 = 3;
const MAX_RECORD_BYTES: usize = 8 * 1024;
const MAX_ROOT_BYTES: usize = 4 * 1024;
const MAX_ROOT_IDENTITY_BYTES: usize = 512;
const MAX_DEBT_ROWS: i64 = 128;
const MAX_ROOT_ROWS: i64 = 32;
const MAX_PAGE_ROWS: i64 = 32;
const MAX_PAGE_COUNT: i64 = 512;
const MAX_DATABASE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ROLLBACK_JOURNAL_BYTES: u64 = 2 * MAX_DATABASE_BYTES;
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
    /// Serialized directory identity for the staged replacement. `None` is
    /// retained for migrated v2 rows that predate inode-reuse-safe staging.
    pub(crate) fresh_store_identity: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoadedDebt {
    pub(crate) payload: Vec<u8>,
    pub(crate) protocol_version: u8,
    pub(crate) fresh_store_identity: Option<Vec<u8>>,
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

/// Return true if the exact override-root path or any component-bounded child
/// has journal history. The query is literal byte-prefix matching, not SQL
/// LIKE, so wildcard bytes and sibling path prefixes cannot broaden it.
pub(crate) fn has_rows_at_or_below_root(resource_db: &Path, cache_root: &Path) -> io::Result<bool> {
    let root = root_path_bytes(cache_root)?;
    let mut prefix = Vec::with_capacity(root.len() + 1);
    prefix.extend_from_slice(root);
    prefix.push(b'/');

    let path = path_for_resource_db(resource_db)?;
    let Some(connection) = open_reader(&path)? else {
        return Ok(false);
    };
    if !ensure_schema(&connection, false)? {
        return Err(invalid_data("rotation journal schema is unsupported"));
    }
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(sqlite_error)?;
    if version != 2 && version != SCHEMA_VERSION {
        return Err(invalid_data("rotation journal schema is unsupported"));
    }

    // A normal root row is the usual case, but a malformed database can hold
    // orphan debt rows with foreign-key checks disabled. Check both tables so
    // orphaned history also blocks initialization of a reused path. The table
    // names are static and every scan is capped before the caller trusts a
    // negative result.
    for (table, row_cap) in [
        ("rotation_root", MAX_ROOT_ROWS),
        ("rotation_debt", MAX_DEBT_ROWS),
    ] {
        let sql = format!(
            "SELECT length(root_path), \
             CASE WHEN length(root_path) BETWEEN 1 AND ?1 THEN root_path ELSE NULL END \
             FROM {table} LIMIT ?2"
        );
        let mut statement = connection.prepare(&sql).map_err(sqlite_error)?;
        let mut rows = statement
            .query(params![MAX_ROOT_BYTES as i64, row_cap + 1])
            .map_err(sqlite_error)?;
        let mut count = 0_i64;
        while let Some(row) = rows.next().map_err(sqlite_error)? {
            count += 1;
            if count > row_cap {
                return Err(invalid_data(
                    "rotation journal root-path row cap is exceeded",
                ));
            }
            let length: i64 = row.get(0).map_err(sqlite_error)?;
            if !(1..=MAX_ROOT_BYTES as i64).contains(&length) {
                return Err(invalid_data(
                    "rotation journal root path exceeds its size bound",
                ));
            }
            let recorded: Vec<u8> = row
                .get::<_, Option<Vec<u8>>>(1)
                .map_err(sqlite_error)?
                .ok_or_else(|| invalid_data("rotation journal root path is unavailable"))?;
            if recorded.as_slice() == root || recorded.starts_with(&prefix) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

struct RotationRootRow {
    root_identity: Vec<u8>,
    generation: i64,
    completed_generation: i64,
    lap_generation: i64,
    cursor: Option<Vec<u8>>,
    lap_incomplete: i64,
    lap_state: i64,
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
    publish_with_protocol(
        resource_db,
        cache_root,
        root_identity,
        repository,
        payload,
        3,
    )
}

/// Recreate a missing external row from an already-published legacy in-cache
/// witness. The row remains protocol 2 until a verified fresh-store proof is
/// adopted; this prevents a migrated/legacy transition from being treated as
/// a transaction that is known to have staged its replacement.
pub(crate) fn publish_legacy(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
    repository: &str,
    payload: &[u8],
) -> io::Result<()> {
    publish_with_protocol(
        resource_db,
        cache_root,
        root_identity,
        repository,
        payload,
        2,
    )
}

fn publish_with_protocol(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
    repository: &str,
    payload: &[u8],
    protocol_version: i64,
) -> io::Result<()> {
    validate_inputs(cache_root, root_identity, repository, payload)?;
    if !matches!(protocol_version, 2 | 3) {
        return Err(invalid_input("rotation protocol version is invalid"));
    }
    let path = path_for_resource_db(resource_db)?;
    let mut connection = open_writer(&path)?;
    ensure_writer_schema(&mut connection)?;
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
            "INSERT INTO rotation_debt(\
                root_path, repository, payload, protocol_version, fresh_store_identity\
             ) VALUES (?1, ?2, ?3, ?4, NULL)",
            params![root, repository, payload, protocol_version],
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
    Ok(
        load_record(resource_db, cache_root, root_identity, repository)?
            .map(|record| record.payload),
    )
}

/// Read one exact row and its durable replacement identity. Schema-v2 rows
/// remain readable as protocol 2 with no replacement identity; callers must
/// apply the narrower legacy recovery rules before adopting them.
pub(crate) fn load_record(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
    repository: &str,
) -> io::Result<Option<LoadedDebt>> {
    validate_root(cache_root, root_identity)?;
    validate_repository(repository)?;
    let path = path_for_resource_db(resource_db)?;
    let Some(mut connection) = open_reader(&path)? else {
        return Ok(None);
    };
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(sqlite_error)?;
    if !ensure_schema(&transaction, false)? {
        return Err(invalid_data("rotation debt journal schema is absent"));
    }
    let root = cache_root.as_os_str().as_bytes();
    let version: i64 = transaction
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(sqlite_error)?;
    let state: Option<RotationRootRow> = transaction
        .query_row(
            "SELECT root_identity, generation, completed_generation, lap_generation, cursor, \
                lap_incomplete, lap_state \
             FROM rotation_root WHERE root_path=?1",
            params![root],
            |row| {
                Ok(RotationRootRow {
                    root_identity: row.get(0)?,
                    generation: row.get(1)?,
                    completed_generation: row.get(2)?,
                    lap_generation: row.get(3)?,
                    cursor: row.get(4)?,
                    lap_incomplete: row.get(5)?,
                    lap_state: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    let debt_count: i64 = transaction
        .query_row(
            "SELECT count(*) FROM rotation_debt WHERE root_path=?1",
            params![root],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    if debt_count > MAX_DEBT_ROWS {
        return Err(invalid_data("rotation journal state is malformed"));
    }
    let row = match version {
        2 => transaction
            .query_row(
                "SELECT length(payload), \
                 CASE WHEN length(payload) BETWEEN 1 AND ?1 THEN payload ELSE NULL END \
                 FROM rotation_debt WHERE root_path=?2 AND repository=?3",
                params![MAX_RECORD_BYTES as i64, root, repository],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                        2_i64,
                        None,
                        None,
                    ))
                },
            )
            .optional()
            .map_err(sqlite_error)?,
        SCHEMA_VERSION => transaction
            .query_row(
                "SELECT length(payload), \
                 CASE WHEN length(payload) BETWEEN 1 AND ?1 THEN payload ELSE NULL END, \
                 protocol_version, length(fresh_store_identity), \
                 CASE WHEN fresh_store_identity IS NULL OR \
                 length(fresh_store_identity) BETWEEN 1 AND ?4 \
                 THEN fresh_store_identity ELSE NULL END \
                 FROM rotation_debt WHERE root_path=?2 AND repository=?3",
                params![
                    MAX_RECORD_BYTES as i64,
                    root,
                    repository,
                    MAX_ROOT_IDENTITY_BYTES as i64,
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<Vec<u8>>>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(sqlite_error)?,
        _ => return Err(invalid_data("rotation debt journal schema is unsupported")),
    };
    let loaded = match row {
        None => None,
        Some((length, payload, protocol_version, identity_length, identity)) => {
            if !(1..=MAX_RECORD_BYTES as i64).contains(&length)
                || !matches!(protocol_version, 2 | 3)
            {
                return Err(invalid_data("rotation debt row exceeds its size limit"));
            }
            let payload =
                payload.ok_or_else(|| invalid_data("rotation debt payload is unavailable"))?;
            let fresh_store_identity = match (identity_length, identity) {
                (None, None) => None,
                (Some(length), Some(identity))
                    if (1..=MAX_ROOT_IDENTITY_BYTES as i64).contains(&length) =>
                {
                    Some(identity)
                }
                _ => {
                    return Err(invalid_data(
                        "rotation debt identity exceeds its size limit",
                    ));
                }
            };
            Some(LoadedDebt {
                payload,
                protocol_version: protocol_version as u8,
                fresh_store_identity,
            })
        }
    };
    let Some(state) = state else {
        if debt_count != 0 {
            return Err(invalid_data(
                "rotation debt rows exist without a root journal record",
            ));
        }
        transaction.commit().map_err(sqlite_error)?;
        return Ok(None);
    };
    validate_root_row(&state)?;
    if state.root_identity != root_identity {
        if root_state_is_clean(&state, debt_count) {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(None);
        }
        return Err(invalid_data(
            "preparation cache root identity differs from its durable journal",
        ));
    }
    transaction.commit().map_err(sqlite_error)?;
    Ok(loaded)
}

/// Advance the durable keyset cursor before the caller acquires any
/// per-repository lease. The SQLite transaction is closed before filesystem
/// recovery, so publisher lock order remains repository lease -> journal.
///
/// Call after a bounded cache-root inventory attempt. Exact journal rows may
/// be processed even when unrelated inventory is incomplete; callers must then
/// mark the lap incomplete and cannot acknowledge it. An active lap resumes its
/// persisted cursor and generation unchanged; an idle or exhausted lap starts
/// at the current generation. Starting a lap never moves `completed_generation`,
/// so pending work remains visible until acknowledged.
pub(crate) fn begin_lap(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
) -> io::Result<u64> {
    validate_root(cache_root, root_identity)?;
    let path = path_for_resource_db(resource_db)?;
    let mut connection = open_writer(&path)?;
    ensure_writer_schema(&mut connection)?;
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
    ensure_writer_schema(&mut connection)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let root = cache_root.as_os_str().as_bytes();
    let generation = ensure_root(&transaction, root, root_identity)?;
    let (lap_generation, cursor, lap_state): (i64, Option<Vec<u8>>, i64) = transaction
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
                "SELECT CAST(repository AS BLOB), length(payload), \
                 length(fresh_store_identity), \
                 CASE WHEN fresh_store_identity IS NULL OR \
                 length(fresh_store_identity) BETWEEN 1 AND 512 \
                 THEN fresh_store_identity ELSE NULL END \
                 FROM rotation_debt \
                 WHERE root_path=?1 AND (?2 IS NULL OR CAST(repository AS BLOB)>?2) \
                 ORDER BY CAST(repository AS BLOB) LIMIT ?3",
            )
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(params![root, cursor, page_rows], |row| {
                let sort_key: Vec<u8> = row.get(0)?;
                let payload_length: i64 = row.get(1)?;
                let identity_length: Option<i64> = row.get(2)?;
                let identity: Option<Vec<u8>> = row.get(3)?;
                let identity_valid = match (identity_length, identity.as_ref()) {
                    (None, None) => true,
                    (Some(length), Some(_)) => {
                        (1..=MAX_ROOT_IDENTITY_BYTES as i64).contains(&length)
                    }
                    _ => false,
                };
                let repository = std::str::from_utf8(&sort_key)
                    .ok()
                    .filter(|value| valid_repository(value))
                    .map(str::to_owned);
                Ok(JournalDebt {
                    repository,
                    sort_key,
                    payload_valid: (1..=MAX_RECORD_BYTES as i64).contains(&payload_length)
                        && identity_valid,
                    fresh_store_identity: identity.filter(|_| identity_valid),
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
    ensure_writer_schema(&mut connection)?;
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
    ensure_writer_schema(&mut connection)?;
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

/// Bind the exact staged replacement identity to a published debt row.
/// `None` may transition to one identity once; an exact retry is harmless,
/// while a different identity is always refused. The caller must already
/// hold the repository lease and have verified the root-level stage proof.
pub(crate) fn record_fresh_store_identity(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
    repository: &str,
    expected_payload: &[u8],
    fresh_store_identity: &[u8],
) -> io::Result<()> {
    validate_inputs(cache_root, root_identity, repository, expected_payload)?;
    if fresh_store_identity.is_empty() || fresh_store_identity.len() > MAX_ROOT_IDENTITY_BYTES {
        return Err(invalid_input(
            "fresh preparation-cache identity is malformed",
        ));
    }
    let path = path_for_resource_db(resource_db)?;
    let mut connection = open_writer(&path)?;
    ensure_writer_schema(&mut connection)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let root = cache_root.as_os_str().as_bytes();
    ensure_root(&transaction, root, root_identity)?;
    let row: Option<(i64, Option<i64>, Option<Vec<u8>>)> = transaction
        .query_row(
            "SELECT payload=?3, length(fresh_store_identity), \
             CASE WHEN fresh_store_identity IS NULL OR \
             length(fresh_store_identity) BETWEEN 1 AND 512 \
             THEN fresh_store_identity ELSE NULL END \
             FROM rotation_debt WHERE root_path=?1 AND repository=?2",
            params![root, repository, expected_payload],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    match row {
        Some((1, Some(length), Some(current)))
            if (1..=MAX_ROOT_IDENTITY_BYTES as i64).contains(&length)
                && current == fresh_store_identity => {}
        Some((1, None, None)) => {
            let changed = transaction
                .execute(
                    "UPDATE rotation_debt SET fresh_store_identity=?3 \
                     WHERE root_path=?1 AND repository=?2 \
                     AND payload=?4 AND fresh_store_identity IS NULL",
                    params![root, repository, fresh_store_identity, expected_payload],
                )
                .map_err(sqlite_error)?;
            if changed != 1 {
                return Err(invalid_data(
                    "rotation debt changed before stage identity binding",
                ));
            }
        }
        Some((1, _, _)) => {
            return Err(invalid_data(
                "rotation debt or staged replacement identity changed",
            ));
        }
        Some(_) => {
            return Err(invalid_data(
                "rotation debt payload changed before stage identity binding",
            ));
        }
        None => {
            return Err(invalid_data(
                "rotation debt disappeared before stage binding",
            ));
        }
    }
    transaction.commit().map_err(sqlite_error)?;
    check_database_size(&path)
}

/// Remove a row only after the caller has completed the pinned filesystem
/// transition and removed the in-cache witness. If a crash occurs first, the
/// external row remains discoverable and the next pass revalidates it.
#[cfg(test)]
fn complete(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
    repository: &str,
    expected_payload: &[u8],
) -> io::Result<()> {
    let record = load_record(resource_db, cache_root, root_identity, repository)?
        .ok_or_else(|| invalid_data("rotation debt row is absent before completion"))?;
    let fresh_identity = record
        .fresh_store_identity
        .ok_or_else(|| invalid_data("rotation debt has no fresh-store identity"))?;
    complete_verified(
        resource_db,
        cache_root,
        root_identity,
        repository,
        expected_payload,
        &fresh_identity,
    )
}

pub(crate) fn complete_verified(
    resource_db: &Path,
    cache_root: &Path,
    root_identity: &[u8],
    repository: &str,
    expected_payload: &[u8],
    expected_fresh_store_identity: &[u8],
) -> io::Result<()> {
    validate_inputs(cache_root, root_identity, repository, expected_payload)?;
    if expected_fresh_store_identity.is_empty()
        || expected_fresh_store_identity.len() > MAX_ROOT_IDENTITY_BYTES
    {
        return Err(invalid_input(
            "fresh preparation-cache identity is malformed",
        ));
    }
    let path = path_for_resource_db(resource_db)?;
    let mut connection = open_writer(&path)?;
    ensure_writer_schema(&mut connection)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let root = cache_root.as_os_str().as_bytes();
    ensure_root(&transaction, root, root_identity)?;
    let current: Option<(i64, Option<i64>, Option<Vec<u8>>)> = transaction
        .query_row(
            "SELECT payload=?3, length(fresh_store_identity), \
             CASE WHEN fresh_store_identity IS NULL OR \
             length(fresh_store_identity) BETWEEN 1 AND 512 \
             THEN fresh_store_identity ELSE NULL END \
             FROM rotation_debt WHERE root_path=?1 AND repository=?2",
            params![root, repository, expected_payload],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    match current {
        Some((1, Some(length), Some(current_identity)))
            if (1..=MAX_ROOT_IDENTITY_BYTES as i64).contains(&length)
                && current_identity == expected_fresh_store_identity =>
        {
            let deleted = transaction
                .execute(
                    "DELETE FROM rotation_debt WHERE root_path=?1 AND repository=?2 \
                     AND payload=?3 AND fresh_store_identity=?4",
                    params![
                        root,
                        repository,
                        expected_payload,
                        expected_fresh_store_identity
                    ],
                )
                .map_err(sqlite_error)?;
            if deleted != 1 {
                return Err(invalid_data("rotation debt changed during completion"));
            }
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
    let root = root_path_bytes(cache_root)?;
    if let Some(identity) = current_root_identity {
        validate_root(cache_root, identity)?;
    }
    let path = path_for_resource_db(resource_db)?;
    let Some(mut connection) = open_reader(&path)? else {
        return Ok(None);
    };
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(sqlite_error)?;
    if !ensure_schema(&transaction, false)? {
        return Err(invalid_data("rotation debt journal schema is absent"));
    }
    let state: Option<RotationRootRow> = transaction
        .query_row(
            "SELECT root_identity, generation, completed_generation, lap_generation, cursor, \
                lap_incomplete, lap_state \
             FROM rotation_root WHERE root_path=?1",
            params![root],
            |row| {
                Ok(RotationRootRow {
                    root_identity: row.get(0)?,
                    generation: row.get(1)?,
                    completed_generation: row.get(2)?,
                    lap_generation: row.get(3)?,
                    cursor: row.get(4)?,
                    lap_incomplete: row.get(5)?,
                    lap_state: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    let debt_count: i64 = transaction
        .query_row(
            "SELECT count(*) FROM rotation_debt WHERE root_path=?1",
            params![root],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    if debt_count > MAX_DEBT_ROWS {
        return Err(invalid_data("rotation journal state is malformed"));
    }
    let probe = match state {
        None => {
            if debt_count > 0 {
                JournalProbe::Unknown
            } else {
                JournalProbe::NoDebt
            }
        }
        Some(state) => {
            validate_root_row(&state)?;
            let pending = !root_state_is_clean(&state, debt_count);
            if current_root_identity
                .is_some_and(|identity| identity != state.root_identity.as_slice())
            {
                if pending {
                    JournalProbe::Unknown
                } else {
                    JournalProbe::NoDebt
                }
            } else if current_root_identity.is_none() && pending {
                JournalProbe::Unknown
            } else if pending {
                JournalProbe::Pending
            } else {
                JournalProbe::NoDebt
            }
        }
    };
    transaction.commit().map_err(sqlite_error)?;
    Ok(Some(probe))
}

/// Detect orphan debt rows using the root-path prefix of the composite
/// primary key. Missing root metadata is no debt only when this indexed
/// lookup also proves that no child rows remain.
fn root_has_debt_rows(connection: &Connection, root: &[u8]) -> io::Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM rotation_debt WHERE root_path=?1)",
            params![root],
            |row| row.get(0),
        )
        .map_err(sqlite_error)
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
    validate_sqlite_sidecars(path, true)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection = Connection::open_with_flags(path, flags).map_err(sqlite_error)?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(sqlite_error)?;

    // Refuse unknown schemas before chmod, page-limit changes, or journal-mode
    // conversion. SQLite may first perform its own bounded rollback recovery.
    validate_schema_before_writer(&connection)?;
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(sqlite_error)?;
    if !journal_mode.eq_ignore_ascii_case("delete") {
        return Err(invalid_data("rotation journal is not in DELETE mode"));
    }

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
    validate_sqlite_sidecars(path, false)?;
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
    validate_sqlite_sidecars(path, true)?;
    if has_rollback_journal(path)? {
        // Read-only SQLite cannot safely recover a hot rollback journal. Run
        // the same bounded/schema-checked writer open once, then reopen read
        // only after SQLite has completed recovery and removed the sidecar.
        drop(open_writer(path)?);
        check_database_size(path)?;
    }
    validate_sqlite_sidecars(path, false)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection = Connection::open_with_flags(path, flags).map_err(sqlite_error)?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(sqlite_error)?;
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(sqlite_error)?;
    if !journal_mode.eq_ignore_ascii_case("delete") {
        return Err(invalid_data("rotation journal is not in DELETE mode"));
    }
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

fn validate_schema_before_writer(connection: &Connection) -> io::Result<()> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(sqlite_error)?;
    if version == SCHEMA_VERSION || version == 2 {
        if ensure_schema(connection, false)? {
            return Ok(());
        }
        return Err(invalid_data("rotation journal schema is unsupported"));
    }
    if version != 0 {
        return Err(invalid_data(
            "rotation journal schema version is unsupported",
        ));
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
    Ok(())
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn has_rollback_journal(path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(sidecar_path(path, "-journal")) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn require_sidecar_absent(path: &Path, suffix: &str) -> io::Result<()> {
    match std::fs::symlink_metadata(sidecar_path(path, suffix)) {
        Ok(_) => Err(invalid_data("unexpected rotation journal SQLite sidecar")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn validate_sqlite_sidecars(path: &Path, allow_rollback_journal: bool) -> io::Result<()> {
    let rollback = sidecar_path(path, "-journal");
    match std::fs::symlink_metadata(&rollback) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
        Ok(metadata) => {
            if !allow_rollback_journal
                || metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.nlink() != 1
                || metadata.uid() != effective_uid()
                || metadata.mode() & 0o077 != 0
                || metadata.len() > MAX_ROLLBACK_JOURNAL_BYTES
            {
                return Err(invalid_data(
                    "rotation journal rollback sidecar identity or size is unsafe",
                ));
            }
        }
    }
    require_sidecar_absent(path, "-wal")?;
    require_sidecar_absent(path, "-shm")
}

fn effective_uid() -> u32 {
    // SAFETY: `geteuid` has no pointer arguments and returns the caller's effective UID.
    unsafe { libc::geteuid() }
}

fn ensure_schema(connection: &Connection, create: bool) -> io::Result<bool> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(sqlite_error)?;
    if version == SCHEMA_VERSION || version == 2 {
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
        if version == SCHEMA_VERSION {
            connection
                .prepare(
                    "SELECT root_path, repository, payload, protocol_version, \
                     fresh_store_identity FROM rotation_debt LIMIT 0",
                )
                .map_err(sqlite_error)?;
        } else {
            connection
                .prepare("SELECT root_path, repository, payload FROM rotation_debt LIMIT 0")
                .map_err(sqlite_error)?;
        }
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
                protocol_version INTEGER NOT NULL CHECK(protocol_version IN (2,3)),
                fresh_store_identity BLOB CHECK(\
                    fresh_store_identity IS NULL OR \
                    length(fresh_store_identity) BETWEEN 1 AND 512\
                ),
                PRIMARY KEY(root_path, repository)
             ) WITHOUT ROWID;
             PRAGMA user_version=3;",
        )
        .map_err(sqlite_error)?;
    transaction.commit().map_err(sqlite_error)?;
    Ok(true)
}

fn ensure_writer_schema(connection: &mut Connection) -> io::Result<()> {
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(sqlite_error)?;
    if version == 2 {
        migrate_v2_schema(connection)?;
    }
    if ensure_schema(connection, true)? {
        Ok(())
    } else {
        Err(invalid_data(
            "rotation journal schema version is unsupported",
        ))
    }
}

fn migrate_v2_schema(connection: &mut Connection) -> io::Result<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let version: i64 = transaction
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(sqlite_error)?;
    match version {
        2 => {
            if !ensure_schema(&transaction, false)? {
                return Err(invalid_data("rotation journal schema is unsupported"));
            }
            transaction
                .execute_batch(
                    "ALTER TABLE rotation_debt ADD COLUMN protocol_version INTEGER NOT NULL \
                         DEFAULT 2 CHECK(protocol_version IN (2,3)); \
                     ALTER TABLE rotation_debt ADD COLUMN fresh_store_identity BLOB \
                         CHECK(fresh_store_identity IS NULL OR \
                               length(fresh_store_identity) BETWEEN 1 AND 512); \
                     PRAGMA user_version=3;",
                )
                .map_err(sqlite_error)?;
        }
        SCHEMA_VERSION => {
            if !ensure_schema(&transaction, false)? {
                return Err(invalid_data("rotation journal schema is unsupported"));
            }
        }
        _ => return Err(invalid_data("rotation journal schema is unsupported")),
    }
    transaction.commit().map_err(sqlite_error)?;
    if !ensure_schema(connection, false)? {
        return Err(invalid_data("rotation journal migration did not complete"));
    }
    Ok(())
}

fn ensure_root(
    transaction: &rusqlite::Transaction<'_>,
    root: &[u8],
    root_identity: &[u8],
) -> io::Result<u64> {
    let existing: Option<RotationRootRow> = transaction
        .query_row(
            "SELECT root_identity, generation, completed_generation, lap_generation, cursor, lap_incomplete, lap_state \
             FROM rotation_root WHERE root_path=?1",
            params![root],
            |row| {
                Ok(RotationRootRow {
                    root_identity: row.get(0)?,
                    generation: row.get(1)?,
                    completed_generation: row.get(2)?,
                    lap_generation: row.get(3)?,
                    cursor: row.get(4)?,
                    lap_incomplete: row.get(5)?,
                    lap_state: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    match existing {
        Some(existing) if existing.root_identity == root_identity => {
            validate_root_row(&existing)?;
            u64::try_from(existing.generation)
                .map_err(|_| invalid_data("negative rotation generation"))
        }
        Some(existing) => {
            validate_root_row(&existing)?;
            let count: i64 = transaction
                .query_row(
                    "SELECT count(*) FROM rotation_debt WHERE root_path=?1",
                    params![root],
                    |row| row.get(0),
                )
                .map_err(sqlite_error)?;
            if count != 0
                || existing.generation != existing.completed_generation
                || existing.lap_generation != existing.generation
                || existing.cursor.is_some()
                || existing.lap_incomplete != 0
                || existing.lap_state != LAP_IDLE
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
            if root_has_debt_rows(transaction, root)? {
                return Err(invalid_data(
                    "rotation debt rows exist without a root journal record",
                ));
            }
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

fn validate_root_row(state: &RotationRootRow) -> io::Result<()> {
    if !(1..=MAX_ROOT_IDENTITY_BYTES).contains(&state.root_identity.len()) {
        return Err(invalid_data("rotation journal root identity is malformed"));
    }
    validate_rotation_state(
        state.generation,
        state.completed_generation,
        state.lap_generation,
        state.cursor.as_deref(),
        state.lap_incomplete,
        state.lap_state,
    )
}

fn root_state_is_clean(state: &RotationRootRow, debt_count: i64) -> bool {
    debt_count == 0
        && state.generation == state.completed_generation
        && state.lap_generation == state.generation
        && state.cursor.is_none()
        && state.lap_incomplete == 0
        && state.lap_state == LAP_IDLE
}

fn check_database_size(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != effective_uid()
        || metadata.mode() & 0o077 != 0
        || metadata.len() > MAX_DATABASE_BYTES
    {
        return Err(invalid_data("rotation journal exceeded its file bound"));
    }
    validate_sqlite_sidecars(path, false)
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
    const TEST_FRESH_IDENTITY: &[u8] = b"fixture-fresh-store-identity";

    struct Fixture {
        _directory: TempDir,
        resource_db: PathBuf,
        cache_root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let fixture_root = directory.path().canonicalize().unwrap();
            let resource_db = fixture_root.join("host-resources.db");
            let cache_root = fixture_root.join("preparation-cache");
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
            record_fresh_store_identity(
                &self.resource_db,
                &self.cache_root,
                ROOT_IDENTITY,
                repository,
                payload,
                TEST_FRESH_IDENTITY,
            )
            .unwrap();
        }

        fn finish_empty_lap_after_completion(&self, repository: &str, payload: &[u8]) {
            complete_verified(
                &self.resource_db,
                &self.cache_root,
                ROOT_IDENTITY,
                repository,
                payload,
                TEST_FRESH_IDENTITY,
            )
            .unwrap();
            let generation = begin_lap(&self.resource_db, &self.cache_root, ROOT_IDENTITY).unwrap();
            let page = next_page(&self.resource_db, &self.cache_root, ROOT_IDENTITY).unwrap();
            assert_eq!(page.generation, generation);
            assert!(page.entries.is_empty());
            assert!(page.lap_exhausted);
            acknowledge_lap(
                &self.resource_db,
                &self.cache_root,
                ROOT_IDENTITY,
                generation,
            )
            .unwrap();
        }
    }

    #[test]
    fn orphan_debt_without_root_record_is_unknown_and_load_refuses_without_mutation() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"debt-a");
        let path = path_for_resource_db(&fixture.resource_db).unwrap();
        let root = fixture.cache_root.as_os_str().as_bytes();

        // Simulate a malformed database created with foreign-key enforcement
        // disabled. The child row remains, while its root row is absent.
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .unwrap();
        connection
            .execute(
                "DELETE FROM rotation_root WHERE root_path=?1",
                params![root],
            )
            .unwrap();
        let orphan_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM rotation_debt WHERE root_path=?1",
                params![root],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphan_count, 1);
        drop(connection);

        let original = std::fs::read(&path).unwrap();
        assert_eq!(
            probe(
                &fixture.resource_db,
                &fixture.cache_root,
                Some(ROOT_IDENTITY)
            )
            .unwrap(),
            Some(JournalProbe::Unknown)
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);

        assert!(
            load(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                REPOSITORY_A
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(
            publish(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                REPOSITORY_B,
                b"new-debt",
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn first_recovery_with_an_absent_cache_root_is_a_noop_without_creating_state() {
        let fixture = Fixture::new();
        std::fs::remove_dir(&fixture.cache_root).unwrap();

        let result = crate::preparation::shared_cache_budget_at_with_resource_db(
            None,
            &fixture.cache_root,
            &fixture.resource_db,
            REPOSITORY_A,
            "test-digest",
        )
        .unwrap();

        assert!(result.is_none());
        assert!(!fixture.cache_root.exists());
        assert!(!path_for_resource_db(&fixture.resource_db).unwrap().exists());
    }

    #[test]
    fn missing_cache_root_with_a_published_debt_remains_unknown_and_untouched() {
        let fixture = Fixture::new();
        let store = fixture.cache_root.join(REPOSITORY_A);
        std::fs::create_dir(&store).unwrap();
        std::fs::write(store.join("payload"), b"must remain pending").unwrap();
        let root_identity =
            crate::preparation::rotation_debt::DirectoryIdentity::capture(&fixture.cache_root)
                .unwrap();
        let debt = crate::preparation::rotation_debt::RotationDebt::new(
            REPOSITORY_A,
            &crate::preparation::rotation_debt::new_transaction_id(),
            root_identity.clone(),
            crate::preparation::rotation_debt::DirectoryIdentity::capture(&store).unwrap(),
        )
        .unwrap();
        let payload = crate::preparation::rotation_debt::encode(&debt).unwrap();
        publish(
            &fixture.resource_db,
            &fixture.cache_root,
            &root_identity.encode().unwrap(),
            REPOSITORY_A,
            &payload,
        )
        .unwrap();
        let moved_root = fixture
            .cache_root
            .parent()
            .unwrap()
            .join("cache-root-with-pending-debt");
        std::fs::rename(&fixture.cache_root, &moved_root).unwrap();

        let result = crate::preparation::rotation_debt::recover_with_journal(
            &fixture.cache_root,
            &fixture.resource_db,
            REPOSITORY_A,
            std::time::Instant::now() + Duration::from_secs(1),
        );

        assert!(result.is_err());
        assert!(!fixture.cache_root.exists());
        assert_eq!(
            std::fs::read(moved_root.join(REPOSITORY_A).join("payload")).unwrap(),
            b"must remain pending"
        );
        assert_eq!(
            probe(&fixture.resource_db, &fixture.cache_root, None).unwrap(),
            Some(JournalProbe::Unknown)
        );
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
    fn v2_writer_migration_preserves_rows_and_is_idempotent() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let path = path_for_resource_db(&fixture.resource_db).unwrap();
        let root = fixture.cache_root.as_os_str().as_bytes();
        let payload = b"legacy-v2-debt";
        {
            let connection = Connection::open(&path).unwrap();
            connection
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
                .unwrap();
            connection
                .execute(
                    "INSERT INTO rotation_root(root_path, root_identity, generation, \
                        completed_generation, lap_generation, cursor, lap_incomplete, lap_state) \
                     VALUES (?1, ?2, 2, 1, 2, NULL, 0, ?3)",
                    params![root, ROOT_IDENTITY, LAP_IDLE],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO rotation_debt(root_path, repository, payload) VALUES (?1, ?2, ?3)",
                    params![root, REPOSITORY_A, payload],
                )
                .unwrap();
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let before = load_record(
            &fixture.resource_db,
            &fixture.cache_root,
            ROOT_IDENTITY,
            REPOSITORY_A,
        )
        .unwrap()
        .expect("v2 row must remain readable before a writer migrates it");
        assert_eq!(before.payload, payload);
        assert_eq!(before.protocol_version, 2);
        assert_eq!(before.fresh_store_identity, None);

        record_fresh_store_identity(
            &fixture.resource_db,
            &fixture.cache_root,
            ROOT_IDENTITY,
            REPOSITORY_A,
            payload,
            TEST_FRESH_IDENTITY,
        )
        .unwrap();
        record_fresh_store_identity(
            &fixture.resource_db,
            &fixture.cache_root,
            ROOT_IDENTITY,
            REPOSITORY_A,
            payload,
            TEST_FRESH_IDENTITY,
        )
        .unwrap();

        let migrated = load_record(
            &fixture.resource_db,
            &fixture.cache_root,
            ROOT_IDENTITY,
            REPOSITORY_A,
        )
        .unwrap()
        .expect("the migrated row remains discoverable");
        assert_eq!(migrated.payload, payload);
        assert_eq!(migrated.protocol_version, 2);
        assert_eq!(
            migrated.fresh_store_identity.as_deref(),
            Some(TEST_FRESH_IDENTITY)
        );
        let connection = open_reader(&path).unwrap().unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let state: (i64, i64, i64) = connection
            .query_row(
                "SELECT generation, completed_generation, lap_generation \
                 FROM rotation_root WHERE root_path=?1",
                params![root],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(state, (2, 1, 2));
        drop(connection);

        assert!(
            record_fresh_store_identity(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                REPOSITORY_A,
                payload,
                b"different-fresh-identity",
            )
            .is_err()
        );
        let after_conflict = load_record(
            &fixture.resource_db,
            &fixture.cache_root,
            ROOT_IDENTITY,
            REPOSITORY_A,
        )
        .unwrap()
        .unwrap();
        assert_eq!(after_conflict.payload, payload);
        assert_eq!(
            after_conflict.fresh_store_identity.as_deref(),
            Some(TEST_FRESH_IDENTITY)
        );
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
                .map(|entry| entry.repository.as_deref())
                .collect::<Vec<_>>(),
            vec![Some(REPOSITORY_A), Some(REPOSITORY_B)]
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
        assert_eq!(
            retry.entries[0].repository.as_deref(),
            Some(low_key.as_str())
        );
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
    fn future_schema_refusal_preserves_database_bytes_and_permissions() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"debt-a");
        let path = path_for_resource_db(&fixture.resource_db).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(connection);
        let original = std::fs::read(&path).unwrap();
        let original_mode = std::fs::metadata(&path).unwrap().mode() & 0o777;

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
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            std::fs::metadata(&path).unwrap().mode() & 0o777,
            original_mode
        );
    }

    #[test]
    fn wal_database_and_sidecar_are_refused_without_mutation() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"debt-a");
        let path = path_for_resource_db(&fixture.resource_db).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        let mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert!(mode.eq_ignore_ascii_case("wal"));
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        connection
            .execute(
                "UPDATE rotation_root SET generation=generation+1 WHERE root_path=?1",
                params![fixture.cache_root.as_os_str().as_bytes()],
            )
            .unwrap();
        let wal_path = sidecar_path(&path, "-wal");
        let wal_bytes = std::fs::read(&wal_path).unwrap();
        assert!(!wal_bytes.is_empty());
        let main_bytes = std::fs::read(&path).unwrap();
        let original_mode = std::fs::metadata(&path).unwrap().mode() & 0o777;

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
        assert_eq!(std::fs::read(&path).unwrap(), main_bytes);
        assert_eq!(std::fs::read(&wal_path).unwrap(), wal_bytes);
        assert_eq!(
            std::fs::metadata(&path).unwrap().mode() & 0o777,
            original_mode
        );
        drop(connection);
    }

    #[test]
    fn oversized_rollback_sidecar_is_refused_without_mutation() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"debt-a");
        let path = path_for_resource_db(&fixture.resource_db).unwrap();
        let rollback = sidecar_path(&path, "-journal");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&rollback)
            .unwrap();
        file.set_len(MAX_ROLLBACK_JOURNAL_BYTES + 1).unwrap();
        drop(file);
        let original = std::fs::read(&path).unwrap();

        assert!(begin_lap(&fixture.resource_db, &fixture.cache_root, ROOT_IDENTITY).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            std::fs::metadata(&rollback).unwrap().len(),
            MAX_ROLLBACK_JOURNAL_BYTES + 1
        );
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
                "INSERT INTO rotation_debt(root_path, repository, payload, protocol_version, fresh_store_identity) VALUES (?1, ?2, ?3, 2, NULL)",
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
    fn clean_replaced_root_is_no_debt_without_rewriting_the_journal() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"debt-a");
        fixture.finish_empty_lap_after_completion(REPOSITORY_A, b"debt-a");
        let path = path_for_resource_db(&fixture.resource_db).unwrap();
        let original = std::fs::read(&path).unwrap();

        assert_eq!(
            load_record(
                &fixture.resource_db,
                &fixture.cache_root,
                b"replacement-root",
                REPOSITORY_A,
            )
            .unwrap(),
            None
        );
        assert_eq!(
            probe(
                &fixture.resource_db,
                &fixture.cache_root,
                Some(b"replacement-root"),
            )
            .unwrap(),
            Some(JournalProbe::NoDebt)
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn replaced_root_with_other_repository_debt_stays_unknown() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"debt-a");
        let path = path_for_resource_db(&fixture.resource_db).unwrap();
        let original = std::fs::read(&path).unwrap();

        assert!(
            load_record(
                &fixture.resource_db,
                &fixture.cache_root,
                b"replacement-root",
                REPOSITORY_B,
            )
            .is_err()
        );
        assert_eq!(
            probe(
                &fixture.resource_db,
                &fixture.cache_root,
                Some(b"replacement-root"),
            )
            .unwrap(),
            Some(JournalProbe::Unknown)
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn replaced_root_with_incomplete_empty_active_lap_stays_unknown() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"debt-a");
        fixture.finish_empty_lap_after_completion(REPOSITORY_A, b"debt-a");
        let path = path_for_resource_db(&fixture.resource_db).unwrap();
        let root = fixture.cache_root.as_os_str().as_bytes();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE rotation_root SET lap_incomplete=1, lap_state=?2 WHERE root_path=?1",
                params![root, LAP_ACTIVE],
            )
            .unwrap();
        drop(connection);
        let original = std::fs::read(&path).unwrap();

        assert!(
            load_record(
                &fixture.resource_db,
                &fixture.cache_root,
                b"replacement-root",
                REPOSITORY_B,
            )
            .is_err()
        );
        assert_eq!(
            probe(
                &fixture.resource_db,
                &fixture.cache_root,
                Some(b"replacement-root"),
            )
            .unwrap(),
            Some(JournalProbe::Unknown)
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn matching_root_allows_exact_repository_miss_with_other_debt() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"debt-a");

        assert_eq!(
            load_record(
                &fixture.resource_db,
                &fixture.cache_root,
                ROOT_IDENTITY,
                REPOSITORY_B,
            )
            .unwrap(),
            None
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

    #[test]
    fn cache_root_journal_probe_is_literal_and_component_bounded() {
        let fixture = Fixture::new();
        let parent = fixture._directory.path();
        let root = parent.join("cache%_literal");
        let child = root.join("nested");
        let sibling = parent.join("cache%_literal-sibling");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&child).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        publish(
            &fixture.resource_db,
            &child,
            ROOT_IDENTITY,
            REPOSITORY_A,
            b"nested debt",
        )
        .unwrap();
        publish(
            &fixture.resource_db,
            &sibling,
            ROOT_IDENTITY,
            REPOSITORY_B,
            b"sibling debt",
        )
        .unwrap();

        assert!(has_rows_at_or_below_root(&fixture.resource_db, &root).unwrap());
        assert!(has_rows_at_or_below_root(&fixture.resource_db, &child).unwrap());
        assert!(!has_rows_at_or_below_root(&fixture.resource_db, &sibling.join("child")).unwrap());
        let no_match = parent.join("cache%_literal-sibling-extra");
        std::fs::create_dir(&no_match).unwrap();
        assert!(!has_rows_at_or_below_root(&fixture.resource_db, &no_match).unwrap());
    }

    #[test]
    fn cache_root_journal_probe_finds_orphan_debt_rows() {
        let fixture = Fixture::new();
        fixture.publish(REPOSITORY_A, b"seed schema");
        let orphan_root = fixture._directory.path().join("orphan-cache");
        std::fs::create_dir(&orphan_root).unwrap();
        let path = path_for_resource_db(&fixture.resource_db).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .unwrap();
        connection
            .execute(
                "INSERT INTO rotation_debt(root_path, repository, payload, protocol_version, fresh_store_identity) \
                 VALUES (?1, ?2, ?3, 3, NULL)",
                params![
                    orphan_root.as_os_str().as_bytes(),
                    REPOSITORY_B,
                    b"orphan debt".as_slice()
                ],
            )
            .unwrap();
        let root_rows: i64 = connection
            .query_row(
                "SELECT count(*) FROM rotation_root WHERE root_path=?1",
                params![orphan_root.as_os_str().as_bytes()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(root_rows, 0);
        drop(connection);

        assert!(has_rows_at_or_below_root(&fixture.resource_db, &orphan_root).unwrap());
        assert!(
            !has_rows_at_or_below_root(
                &fixture.resource_db,
                &fixture._directory.path().join("other-cache")
            )
            .unwrap()
        );
    }
}
