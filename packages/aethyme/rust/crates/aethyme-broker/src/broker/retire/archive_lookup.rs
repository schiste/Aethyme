//! Which recovery archive a retired checkout comes back from, for a restore
//! that falls back to the session branch when there is none: "none" has to
//! be proven, because the branch holds none of the archived changes.

use super::*;
use crate::cleanup_resolve::{CheckedArchiveRoot, MANIFEST_FILE};

/// The recovery archive of a session, as far as it can be told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArchiveLookup {
    Found(PathBuf),
    /// Every place an archive may be in was read and holds none, and every
    /// archive named for the session is proven gone.
    NoneProven,
    /// It cannot be told; the reason.
    Unknown(String),
}

/// Whether the archive named `archive` is proven gone: it is plainly missing
/// from a directory that is still there (a volume that is not mounted has
/// neither), or only its empty directory is left (removal deletes the
/// manifest last). Anything that cannot be read proves nothing.
pub(super) fn archive_proven_gone(archive: &Path) -> bool {
    match std::fs::symlink_metadata(archive) {
        Ok(metadata) => {
            metadata.is_dir()
                && std::fs::read_dir(archive).is_ok_and(|mut entries| entries.next().is_none())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            archive.parent().is_some_and(Path::is_dir)
        }
        Err(_) => false,
    }
}

/// Whether `archive` is there (`Some(true)`), proven gone (`Some(false)`) or
/// neither. A directory with content but no manifest is neither.
fn archive_present(archive: &Path) -> Option<bool> {
    match std::fs::metadata(archive.join(MANIFEST_FILE)) {
        Ok(metadata) => metadata.is_file().then_some(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            archive_proven_gone(archive).then_some(false)
        }
        Err(_) => None,
    }
}

/// Whether `archive` is named as a finished archive of `session_id`.
fn named_for_session(archive: &Path, session_id: i64) -> bool {
    archive
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.starts_with(&format!("{session_id}-")) && !name.ends_with(".partial")
        })
}

impl Broker {
    /// The archive a restore of `session_id`'s checkout must use: the one its
    /// tombstone names (`named`), else the one recorded with the session,
    /// else the newest in either archive directory. An archive that is named
    /// but can be neither read nor proven gone is `Unknown`, unless it turns
    /// up under the same name in an archive directory. Operation archives,
    /// which restore does not read, are never returned.
    pub(crate) fn lookup_recovery_archive(
        &self,
        session_id: i64,
        named: Option<&Path>,
    ) -> ArchiveLookup {
        let _span = crate::phase_telemetry::span("checkout_restore_archive_lookup");
        match self.lookup_recovery_archive_inner(session_id, named) {
            Ok(Some(archive)) => ArchiveLookup::Found(archive),
            Ok(None) => ArchiveLookup::NoneProven,
            Err(reason) => ArchiveLookup::Unknown(reason),
        }
    }

    fn lookup_recovery_archive_inner(
        &self,
        session_id: i64,
        named: Option<&Path>,
    ) -> Result<Option<PathBuf>, String> {
        let mut unproven = Vec::new();
        if let Some(named) = named {
            if !named_for_session(named, session_id) || is_operation_archive(named) {
                return Err(format!(
                    "its tombstone names {}, which is not a recovery archive of session {session_id}",
                    named.display()
                ));
            }
            match archive_present(named) {
                Some(true) => return Ok(Some(named.to_path_buf())),
                Some(false) => {}
                None => unproven.push(named.to_path_buf()),
            }
        }
        let recorded = self
            .recorded_archive(session_id)
            .ok_or("the record of archived sessions cannot be read")?
            .filter(|recorded| {
                !is_operation_archive(recorded) && Some(recorded.as_path()) != named
            });
        if let Some(recorded) = recorded {
            match archive_present(&recorded) {
                // Never another archive in place of the one the tombstone
                // names, except a copy of it under the same name.
                Some(true)
                    if unproven
                        .iter()
                        .all(|missing| missing.file_name() == recorded.file_name()) =>
                {
                    return Ok(Some(recorded));
                }
                Some(_) => {}
                None => unproven.push(recorded),
            }
        }
        let roots = self
            .checked_recovery_archive_roots()
            .ok_or("a directory its archive may be in cannot be resolved")?;
        // A named archive in an archive directory that was never made (or was
        // removed whole) below a directory that is still there is gone.
        unproven.retain(|archive| {
            !roots
                .iter()
                .any(|root| archive.parent() == Some(root.path.as_path()) && root.plainly_absent())
        });
        let found = session_archives(&roots, session_id, false)?;
        if let Some(missing) = unproven.first() {
            return found
                .into_iter()
                .find(|archive| {
                    unproven
                        .iter()
                        .all(|missing| missing.file_name() == archive.file_name())
                })
                .map(Some)
                .ok_or_else(|| {
                    format!(
                        "its recovery archive {} can be neither read nor proven gone",
                        missing.display()
                    )
                });
        }
        Ok(found.into_iter().next_back())
    }

    /// The archive recorded with `session_id`, if any; `None` when the record
    /// cannot be read.
    pub(super) fn recorded_archive(&self, session_id: i64) -> Option<Option<PathBuf>> {
        let Some(text) = self.store_ref().meta_get(ARCHIVED_SESSIONS_KEY).ok()? else {
            return Some(None);
        };
        let recorded: std::collections::BTreeMap<i64, String> = serde_json::from_str(&text).ok()?;
        Some(recorded.get(&session_id).map(PathBuf::from))
    }

    /// The newest recovery archive written for `session_id`, if one is found.
    #[cfg(all(test, unix))]
    pub(crate) fn newest_recovery_archive(&self, session_id: i64) -> Option<PathBuf> {
        match self.lookup_recovery_archive(session_id, None) {
            ArchiveLookup::Found(archive) => Some(archive),
            ArchiveLookup::NoneProven | ArchiveLookup::Unknown(_) => None,
        }
    }

    /// The newest operation archive written for `session_id`, if any.
    #[cfg(all(test, unix))]
    pub(crate) fn newest_operation_archive(&self, session_id: i64) -> Option<PathBuf> {
        session_archives(&self.checked_recovery_archive_roots()?, session_id, true)
            .ok()?
            .pop()
    }
}

/// The finished archives of `session_id` in `roots` (a directory whose name
/// starts with the session id and that holds a manifest), oldest first; the
/// reason when a root, an entry or a manifest cannot be read.
fn session_archives(
    roots: &[CheckedArchiveRoot],
    session_id: i64,
    operation: bool,
) -> Result<Vec<PathBuf>, String> {
    let mut archives = Vec::new();
    for root in roots {
        let unreadable = |error: &std::io::Error| {
            format!(
                "the archive directory {} cannot be read ({error})",
                root.path.display()
            )
        };
        let Some(listing) = root.read().map_err(|error| unreadable(&error))? else {
            continue;
        };
        for entry in listing {
            let archive = entry.map_err(|error| unreadable(&error))?.path();
            if !named_for_session(&archive, session_id)
                || is_operation_archive(&archive) != operation
            {
                continue;
            }
            // Markers beside an archive share its name.
            match std::fs::metadata(&archive) {
                Ok(metadata) if !metadata.is_dir() => continue,
                Ok(_) => {}
                Err(error) => return Err(unreadable(&error)),
            }
            match archive_present(&archive) {
                Some(true) => {}
                Some(false) => continue,
                None => {
                    return Err(format!(
                        "the manifest of {} cannot be read",
                        archive.display()
                    ));
                }
            }
            let written = std::fs::metadata(archive.join(MANIFEST_FILE))
                .and_then(|metadata| metadata.modified())
                .map_err(|error| unreadable(&error))?;
            archives.push((written, archive));
        }
    }
    archives.sort();
    Ok(archives.into_iter().map(|(_, archive)| archive).collect())
}
