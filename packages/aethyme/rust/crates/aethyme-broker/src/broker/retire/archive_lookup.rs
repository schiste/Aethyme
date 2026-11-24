//! Which recovery archive a retired checkout comes back from, for a restore
//! that falls back to the session branch when there is none: "none" has to
//! be proven, because the branch holds none of the archived changes.

use super::*;
use crate::cleanup_resolve::{CheckedArchiveRoot, MANIFEST_FILE};
use crate::store::repository_binding::RepositoryWorktreeBindingState;

/// The recovery archive of a session, as far as it can be told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArchiveLookup {
    Found(PathBuf),
    /// Every place an archive may be in was read and holds none, and every
    /// archive named for the session is proven gone.
    NoneProven,
    Unknown(UnknownArchive),
}

/// Why it cannot be told, and what makes it tellable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnknownArchive {
    pub(crate) reason: String,
    pub(crate) next: String,
}

impl UnknownArchive {
    fn new(reason: impl Into<String>, next: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            next: next.into(),
        }
    }

    fn unreachable(reason: impl Into<String>) -> Self {
        Self::new(
            reason,
            "Make the volume or directory it is on available and readable",
        )
    }

    /// An archive directory cannot be resolved, with the worktree-root
    /// binding in `state`.
    pub(super) fn unresolved_roots(state: Option<RepositoryWorktreeBindingState>) -> Self {
        let reason = "a directory its archive may be in cannot be resolved";
        match state {
            Some(
                RepositoryWorktreeBindingState::FreshReserved
                | RepositoryWorktreeBindingState::RecoveryRequired,
            ) => Self::new(
                format!("{reason}: the worktree-root binding is incomplete"),
                "Review `aethyme broker advanced worktree-root recovery plan`, then run `aethyme broker advanced worktree-root recovery apply --confirm <its digest>`",
            ),
            _ => Self::unreachable(reason),
        }
    }

    /// `archive` is neither present nor proven gone.
    fn archive(archive: &Path) -> Self {
        let manifest = std::fs::symlink_metadata(archive.join(MANIFEST_FILE));
        let damaged = match manifest {
            Ok(metadata) => !metadata.is_file(),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound && archive.is_dir(),
        };
        if damaged {
            return Self::new(
                format!(
                    "its recovery archive {} has no {MANIFEST_FILE}",
                    archive.display()
                ),
                format!(
                    "Put {MANIFEST_FILE} back, or copy out what you need and move that archive out of its directory"
                ),
            );
        }
        Self::unreachable(format!(
            "its recovery archive {} can be neither read nor proven gone",
            archive.display()
        ))
    }
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
    ) -> Result<Option<PathBuf>, UnknownArchive> {
        let mut unproven = Vec::new();
        if let Some(named) = named {
            if !named_for_session(named, session_id) || is_operation_archive(named) {
                return Err(UnknownArchive::new(
                    format!(
                        "its tombstone names {}, which is not a recovery archive of session {session_id}",
                        named.display()
                    ),
                    format!(
                        "Correct the `archive:` line in its {}, or move the tombstone's directory aside",
                        super::super::tombstone::TOMBSTONE_FILE
                    ),
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
            .map_err(|error| self.unknown_record(error))?
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
        let roots = self.checked_recovery_archive_roots().ok_or_else(|| {
            UnknownArchive::unresolved_roots(
                self.store_ref()
                    .repository_worktree_binding()
                    .ok()
                    .map(|binding| binding.state),
            )
        })?;
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
                .ok_or_else(|| UnknownArchive::archive(missing));
        }
        Ok(found.into_iter().next_back())
    }

    fn unknown_record(&self, error: RecordUnreadable) -> UnknownArchive {
        match error {
            RecordUnreadable::Database => UnknownArchive::new(
                "the broker database cannot be read",
                "Wait until no other command holds it",
            ),
            RecordUnreadable::Text(key) => {
                let database = shell_quote(&self.store_ref().db_path().to_string_lossy());
                UnknownArchive::new(
                    "the record of archived sessions cannot be read",
                    format!(
                        "It starts afresh at the next retirement through an archive, but a session closed before that stays refused. Read what it holds with `sqlite3 -readonly {database} \"SELECT value FROM meta WHERE key = '{key}'\"`; if this session has an archive, restore it by hand as its README.md describes. Once the record can be read"
                    ),
                )
            }
        }
    }

    /// The archive recorded with `session_id`, if any. Where the record was
    /// unreadable once and started afresh, its preserved texts are asked too,
    /// for a session closed before that: it can have been recorded in them.
    pub(super) fn recorded_archive(
        &self,
        session_id: i64,
    ) -> Result<Option<PathBuf>, RecordUnreadable> {
        let read = |key| {
            self.store_ref()
                .meta_get(key)
                .map_err(|_| RecordUnreadable::Database)
        };
        if let Some(text) = read(ARCHIVED_SESSIONS_KEY)?
            && let Some(archive) = entry_in(&text, session_id)
                .map_err(|()| RecordUnreadable::Text(ARCHIVED_SESSIONS_KEY))?
        {
            return Ok(Some(archive));
        }
        let Some(preserved) = read(UNREADABLE_RECORDS_KEY)? else {
            return Ok(None);
        };
        let unreadable = || RecordUnreadable::Text(UNREADABLE_RECORDS_KEY);
        let preserved: Vec<UnreadableRecord> =
            serde_json::from_str(&preserved).map_err(|_| unreadable())?;
        // Closed, not cleaned: a lost cleaned record is written again later.
        let closed_at = self
            .store_ref()
            .session(session_id)
            .ok()
            .and_then(|session| session.closed_at);
        for record in preserved.iter().rev() {
            if closed_at.is_some_and(|closed_at| closed_at > record.at_ms) {
                continue;
            }
            if let Some(archive) = entry_in(&record.text, session_id).map_err(|()| unreadable())? {
                return Ok(Some(archive));
            }
        }
        Ok(None)
    }

    /// Record the archive `session_id` was retired through. A record that
    /// cannot be read is preserved beside it first, then started afresh with
    /// the entries that can be.
    pub(super) fn record_session_archive(
        &self,
        session_id: i64,
        archive: &Path,
    ) -> Result<(), String> {
        let archive = archive.to_string_lossy().into_owned();
        let store = self.store_ref();
        for _ in 0..32 {
            let current = store
                .meta_get(ARCHIVED_SESSIONS_KEY)
                .map_err(|error| error.to_string())?;
            let mut restarted = false;
            let mut stored: std::collections::BTreeMap<i64, String> = match current.as_deref() {
                None => Default::default(),
                Some(text) => match serde_json::from_str(text) {
                    Ok(stored) => stored,
                    Err(_) => {
                        preserve_unreadable_record(store, text)?;
                        restarted = true;
                        readable_entries(text)
                    }
                },
            };
            stored.insert(session_id, archive.clone());
            while stored.len() > ARCHIVED_SESSIONS_MAX {
                stored.pop_first();
            }
            let next = serde_json::to_string(&stored).map_err(|error| error.to_string())?;
            if store
                .meta_compare_and_set(ARCHIVED_SESSIONS_KEY, current.as_deref(), &next)
                .map_err(|error| error.to_string())?
            {
                if restarted {
                    crate::phase_telemetry::add(
                        "checkout_retirement_archive_record_restarted",
                        std::time::Duration::ZERO,
                    );
                }
                return Ok(());
            }
        }
        Err(format!(
            "{ARCHIVED_SESSIONS_KEY} kept changing under concurrent writers"
        ))
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

/// Meta key: texts of [`ARCHIVED_SESSIONS_KEY`] that could not be read when
/// the record started afresh (JSON: a list of [`UnreadableRecord`]).
pub(super) const UNREADABLE_RECORDS_KEY: &str = "checkout_retirement.archived_sessions.unreadable";
/// Preserved texts at most; with that many different ones the record is left
/// as it is.
pub(super) const UNREADABLE_RECORDS_MAX: usize = 4;
/// The longest text preserved.
const UNREADABLE_RECORD_MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct UnreadableRecord {
    /// When it was last replaced (epoch ms): nothing closed later is in it.
    pub(super) at_ms: i64,
    pub(super) text: String,
}

/// Why a session's recorded archive cannot be told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecordUnreadable {
    Database,
    /// The value of this meta key.
    Text(&'static str),
}

/// The entries of a record that is a JSON object of session ids, whatever
/// their values; `None` for anything else (a truncated text, another format),
/// which may hold any session.
fn record_entries(text: &str) -> Option<Vec<(i64, serde_json::Value)>> {
    let serde_json::Value::Object(entries) = serde_json::from_str(text).ok()? else {
        return None;
    };
    entries
        .into_iter()
        .map(|(key, value)| {
            let id = key.parse::<i64>().ok().filter(|id| id.to_string() == key)?;
            Some((id, value))
        })
        .collect()
}

/// The archive `text` records for `session_id`, if any; an error when that
/// cannot be told.
fn entry_in(text: &str, session_id: i64) -> Result<Option<PathBuf>, ()> {
    if let Ok(stored) = serde_json::from_str::<std::collections::BTreeMap<i64, String>>(text) {
        return Ok(stored.get(&session_id).map(PathBuf::from));
    }
    match record_entries(text)
        .ok_or(())?
        .into_iter()
        .find(|(id, _)| *id == session_id)
    {
        None => Ok(None),
        Some((_, serde_json::Value::String(archive))) => Ok(Some(PathBuf::from(archive))),
        Some(_) => Err(()),
    }
}

fn readable_entries(text: &str) -> std::collections::BTreeMap<i64, String> {
    record_entries(text)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(id, value)| match value {
            serde_json::Value::String(archive) => Some((id, archive)),
            _ => None,
        })
        .collect()
}

/// Keep `text` beside the record before it is replaced. Every different text
/// is kept; one more than [`UNREADABLE_RECORDS_MAX`] is refused.
fn preserve_unreadable_record(store: &crate::BrokerStore, text: &str) -> Result<(), String> {
    let refused = |reason: &str| {
        crate::phase_telemetry::add(
            "checkout_retirement_archive_record_preserve_refused",
            std::time::Duration::ZERO,
        );
        Err(format!("the record cannot be read and {reason}"))
    };
    if text.len() > UNREADABLE_RECORD_MAX_BYTES {
        return refused("is too long to preserve");
    }
    for _ in 0..32 {
        let current = store
            .meta_get(UNREADABLE_RECORDS_KEY)
            .map_err(|error| error.to_string())?;
        let mut preserved: Vec<UnreadableRecord> = match current.as_deref() {
            None => Vec::new(),
            Some(text) => match serde_json::from_str(text) {
                Ok(preserved) => preserved,
                Err(_) => return refused("neither can its preserved copies"),
            },
        };
        preserved.retain(|record| record.text != text);
        if preserved.len() >= UNREADABLE_RECORDS_MAX {
            return refused("too many copies are preserved already");
        }
        preserved.push(UnreadableRecord {
            at_ms: crate::clock::epoch_ms(),
            text: text.to_string(),
        });
        let next = serde_json::to_string(&preserved).map_err(|error| error.to_string())?;
        if store
            .meta_compare_and_set(UNREADABLE_RECORDS_KEY, current.as_deref(), &next)
            .map_err(|error| error.to_string())?
        {
            return Ok(());
        }
    }
    Err(format!(
        "{UNREADABLE_RECORDS_KEY} kept changing under concurrent writers"
    ))
}

/// The finished archives of `session_id` in `roots` (a directory whose name
/// starts with the session id and that holds a manifest), oldest first; the
/// reason when a root, an entry or a manifest cannot be read.
fn session_archives(
    roots: &[CheckedArchiveRoot],
    session_id: i64,
    operation: bool,
) -> Result<Vec<PathBuf>, UnknownArchive> {
    let mut archives = Vec::new();
    for root in roots {
        let unreadable = |error: &std::io::Error| {
            UnknownArchive::unreachable(format!(
                "the archive directory {} cannot be read ({error})",
                root.path.display()
            ))
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
                None => return Err(UnknownArchive::archive(&archive)),
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
