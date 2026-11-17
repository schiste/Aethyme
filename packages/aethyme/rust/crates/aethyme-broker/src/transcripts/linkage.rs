//! Which broker sessions a transcript belongs to.
//!
//! A `session.holder_bound` event records the holder's pid, start time and
//! command; for most sessions the command names the agent session id
//! (`codex … resume <uuid>`). A transcript's own `cwd` usually names the main
//! repository, rarely the worktree, so it finds a broker database but never
//! identifies a session. Links therefore come from three places, all into the
//! lane's index: the holder's command and, on Linux, its open transcript
//! files when it binds ([`note_holder_bound`]); the same commands read lazily
//! from every known broker database ([`backfill`]); and, while the lane runs,
//! the open transcript files of every live holder of an open session
//! ([`link_live_holders`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use super::eligibility::{CheckoutState, Linkage, LinkedSession};
use super::index::{Index, Link};
use super::open_files::{OpenFileProbe, OpenFiles};
use super::providers::{self, Layout, Provider};

/// Holder events one backfill reads from one database per run.
pub(crate) const BACKFILL_BATCH: i64 = 50_000;
/// How far up from a transcript's `cwd` a broker database is looked for.
const MAX_ANCESTORS: usize = 64;

pub(crate) const SOURCE_COMMAND: &str = "holder_command";
pub(crate) const SOURCE_OPEN_FILES: &str = "holder_open_files";

/// The broker database governing `directory`: the nearest ancestor holding
/// `.aethyme/broker.db`.
pub(crate) fn broker_db_for_directory(directory: &Path) -> Option<PathBuf> {
    directory
        .ancestors()
        .take(MAX_ANCESTORS)
        .map(|ancestor| ancestor.join(crate::BROKER_DB_RELPATH))
        .find(|candidate| candidate.is_file())
        .map(|candidate| fs::canonicalize(&candidate).unwrap_or(candidate))
}

/// The broker databases a pass reads, and whether the list may be missing
/// some.
#[derive(Debug, Default)]
pub(crate) struct KnownDatabases {
    pub databases: Vec<PathBuf>,
    pub warnings: Vec<String>,
    /// The index or the worktree registry could not be read completely, so
    /// a database that links some transcript may be missing.
    pub index_unreadable: bool,
}

/// Every broker database known on this host: configured repositories, the
/// host's worktree registry, databases the index already names, and the
/// databases governing `cwds`.
pub(crate) fn known_broker_dbs(
    repositories: &[PathBuf],
    index: &Index,
    cwds: &BTreeSet<PathBuf>,
    use_registry: bool,
) -> KnownDatabases {
    let mut found = BTreeSet::new();
    let mut warnings = Vec::new();
    let mut index_unreadable = false;
    let mut missing = 0;
    for repository in repositories {
        match broker_db_for_directory(repository) {
            Some(db) => {
                found.insert(db);
            }
            None => missing += 1,
        }
    }
    if missing > 0 {
        // Counted, never named: logs carry no path outside the provider roots.
        warnings.push(format!(
            "{missing} configured repository(ies) have no broker database"
        ));
    }
    if use_registry && let Ok(host_db) = crate::host_operations::default_host_operation_db_path() {
        let inventory = crate::worktree_registry::inventory(
            &host_db,
            Some(std::time::Instant::now() + Duration::from_secs(5)),
        );
        if !inventory.complete {
            index_unreadable = true;
            warnings.push(format!(
                "the worktree registry could not be read completely ({} issue(s))",
                inventory.issues.len()
            ));
        }
        for record in inventory.roots {
            if let Some(db) = broker_db_for_directory(&record.repository_root) {
                found.insert(db);
            }
        }
    }
    match index.known_broker_dbs() {
        Ok(known) => {
            for db in known.into_iter().filter(|db| db.is_file()) {
                found.insert(db);
            }
        }
        Err(error) => {
            index_unreadable = true;
            warnings.push(format!("transcript index unreadable: {error}"));
        }
    }
    for cwd in cwds {
        if let Some(db) = broker_db_for_directory(cwd) {
            found.insert(db);
        }
    }
    KnownDatabases {
        databases: found.into_iter().collect(),
        warnings,
        index_unreadable,
    }
}

/// Open a broker database for reading only; never migrates or writes it.
pub(crate) fn open_broker_db(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("cannot open a broker database: {error}"))?;
    conn.busy_timeout(Duration::from_secs(2))
        .map_err(|error| format!("cannot open a broker database: {error}"))?;
    Ok(conn)
}

/// Links named by one holder's command: every standalone UUID in it.
pub(crate) fn links_from_command(broker_db: &Path, session_id: i64, command: &str) -> Vec<Link> {
    providers::uuids_in(command)
        .into_iter()
        .map(|uuid| Link {
            broker_db: broker_db.to_path_buf(),
            session_id,
            agent_session_id: Some(uuid),
            transcript_path: None,
            source: SOURCE_COMMAND,
        })
        .collect()
}

/// A link for a transcript file a session's holder has open.
fn link_for_open_file(broker_db: &Path, session_id: i64, provider: Provider, path: &Path) -> Link {
    Link {
        broker_db: broker_db.to_path_buf(),
        session_id,
        agent_session_id: path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| providers::session_id_from_file_name(provider, name)),
        transcript_path: Some(path.to_path_buf()),
        source: SOURCE_OPEN_FILES,
    }
}

/// Read holder events newer than the stored cursor from one broker database
/// into the index. Returns the number of new links.
pub(crate) fn backfill(index: &Index, broker_db: &Path, now_ms: i64) -> Result<u64, String> {
    let conn = open_broker_db(broker_db)?;
    let cursor = index.backfill_cursor(broker_db)?;
    let unreadable = |error: rusqlite::Error| format!("cannot read a broker database: {error}");
    let mut statement = conn
        .prepare(
            "SELECT id, session_id, payload_json FROM events
             WHERE kind = ?1 AND id > ?2 ORDER BY id LIMIT ?3",
        )
        .map_err(unreadable)?;
    let rows = statement
        .query_map(
            rusqlite::params![crate::events::SESSION_HOLDER_BOUND, cursor, BACKFILL_BATCH],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(unreadable)?;
    let mut last = cursor;
    let mut added = 0;
    for row in rows {
        let (id, session_id, payload) = row.map_err(unreadable)?;
        last = last.max(id);
        let (Some(session_id), Some(payload)) = (session_id, payload) else {
            continue;
        };
        let Some(command) = serde_json::from_str::<serde_json::Value>(&payload)
            .ok()
            .and_then(|value| {
                value
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .map(String::from)
            })
        else {
            continue;
        };
        for link in links_from_command(broker_db, session_id, &command) {
            if index.add_link(&link, now_ms)? {
                added += 1;
            }
        }
    }
    if last > cursor {
        index.set_backfill_cursor(broker_db, last)?;
    }
    Ok(added)
}

/// The latest holder pid of every open session in one broker database.
fn open_session_holders(conn: &Connection) -> Result<Vec<(i64, u32)>, String> {
    let mut statement = conn
        .prepare(
            "SELECT s.id, (SELECT e.payload_json FROM events e
                           WHERE e.kind = ?1 AND e.session_id = s.id
                           ORDER BY e.id DESC LIMIT 1)
             FROM sessions s WHERE s.cleanup_state = 'open'",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([crate::events::SESSION_HOLDER_BOUND], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut holders = Vec::new();
    for row in rows {
        let (session_id, payload) = row.map_err(|error| error.to_string())?;
        let pid = payload
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
            .and_then(|value| value.get("pid").and_then(serde_json::Value::as_u64))
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid > 0);
        if let Some(pid) = pid {
            holders.push((session_id, pid));
        }
    }
    Ok(holders)
}

/// What linking live holders did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LiveLinks {
    pub added: u64,
    /// A database, its open sessions, or an open-file listing could not be
    /// read: some live holder's transcripts may be unlinked.
    pub incomplete: bool,
}

/// Link every transcript a live holder of an open session has open.
pub(crate) fn link_live_holders<P: OpenFileProbe>(
    index: &Index,
    broker_dbs: &[PathBuf],
    layout: &Layout,
    providers: &[Provider],
    open_files: &mut OpenFiles<P>,
    now_ms: i64,
) -> Result<LiveLinks, String> {
    let mut live = LiveLinks::default();
    let mut by_pid: BTreeMap<u32, Vec<(PathBuf, i64)>> = BTreeMap::new();
    for db in broker_dbs {
        let Ok(conn) = open_broker_db(db) else {
            live.incomplete = true;
            continue;
        };
        let Ok(holders) = open_session_holders(&conn) else {
            live.incomplete = true;
            continue;
        };
        for (session_id, pid) in holders {
            by_pid
                .entry(pid)
                .or_default()
                .push((db.clone(), session_id));
        }
    }
    if by_pid.is_empty() {
        return Ok(live);
    }
    for provider in providers {
        let Some(root) = layout.open_root(*provider) else {
            continue;
        };
        let Ok(holders) = open_files.holders(&root) else {
            live.incomplete = true;
            continue;
        };
        for open in holders {
            let Some(sessions) = by_pid.get(&open.pid) else {
                continue;
            };
            for (db, session_id) in sessions {
                let link = link_for_open_file(db, *session_id, *provider, &open.path);
                if index.add_link(&link, now_ms)? {
                    live.added += 1;
                }
            }
        }
    }
    Ok(live)
}

/// Read-only connections to broker databases, opened once per run.
#[derive(Default)]
pub(crate) struct BrokerDbs {
    open: BTreeMap<PathBuf, Option<Connection>>,
}

impl BrokerDbs {
    fn connection(&mut self, path: &Path) -> Option<&Connection> {
        self.open
            .entry(path.to_path_buf())
            .or_insert_with(|| open_broker_db(path).ok())
            .as_ref()
    }

    /// What the broker databases say about the sessions in `links`.
    pub fn linkage(&mut self, links: &[(PathBuf, i64)]) -> Linkage {
        if links.is_empty() {
            return Linkage::Unlinked;
        }
        let mut sessions = Vec::new();
        for (db, session_id) in links {
            let Some(conn) = self.connection(db) else {
                return Linkage::Unreadable;
            };
            let row = conn
                .query_row(
                    "SELECT cleanup_state, worktree_path FROM sessions WHERE id = ?1",
                    [session_id],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
                )
                .optional();
            let Ok(Some((cleanup_state, worktree_path))) = row else {
                return Linkage::Unreadable;
            };
            sessions.push(LinkedSession {
                cleanup_state,
                checkout: checkout_state(Path::new(&worktree_path)),
            });
        }
        Linkage::Linked(sessions)
    }
}

/// Whether a checkout directory exists. Never follows a symbolic link: a
/// link left where a checkout was still counts as present.
pub(crate) fn checkout_state(path: &Path) -> CheckoutState {
    match fs::symlink_metadata(path) {
        Ok(_) => CheckoutState::Present,
        Err(error) if error.kind() == io::ErrorKind::NotFound => CheckoutState::Absent,
        Err(_) => CheckoutState::Unknown,
    }
}

/// Record the links a newly bound holder implies. Best effort and silent:
/// a failure here only means the lane learns the link later (backfill) or
/// treats the transcript as unlinked. Does nothing at all unless archiving
/// is enabled on this host.
pub(crate) fn note_holder_bound(
    broker_db: &Path,
    session_id: i64,
    holder: &crate::session_holder::AgentProcess,
) {
    if crate::gc::background_process_is_suppressed() {
        return;
    }
    // Read once per process; disabled returns before touching anything.
    let Some((environment, config)) = super::host_settings() else {
        return;
    };
    if !config.archive {
        return;
    }
    let broker_db = fs::canonicalize(broker_db).unwrap_or_else(|_| broker_db.to_path_buf());
    #[allow(unused_mut)]
    let mut links = links_from_command(&broker_db, session_id, &holder.command);
    // Other hosts have no cheap per-process descriptor listing on this path;
    // the lane links those holders through its own open-file listing.
    #[cfg(target_os = "linux")]
    {
        if let Ok(pid) = u32::try_from(holder.pid)
            && pid > 0
        {
            let layout = environment.layout(config);
            for provider in &config.providers {
                let Some(root) = layout.open_root(*provider) else {
                    continue;
                };
                for path in super::open_files::pid_open_under(Path::new("/proc"), pid, &root) {
                    links.push(link_for_open_file(&broker_db, session_id, *provider, &path));
                }
            }
        }
    }
    if links.is_empty() {
        return;
    }
    let Ok(index) = Index::open(&environment.host_dir) else {
        return;
    };
    let now_ms = super::now_ms();
    for link in &links {
        super::best_effort(index.add_link(link, now_ms));
    }
}
