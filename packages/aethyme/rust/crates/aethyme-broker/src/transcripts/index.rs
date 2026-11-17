//! `<host state>/transcripts/index.sqlite3`: the lane's own small database.
//!
//! It holds three things: which agent session ids and transcript paths a
//! broker session was linked to (recorded when a holder binds, and backfilled
//! lazily from `session.holder_bound` events), one row per archived
//! transcript (where it came from, where it went, its SHA-256), and the
//! lane's lifetime counters. It is separate from every repository's broker
//! database on purpose: linking is host-wide, and no broker schema changes.
//! Every table is created with `IF NOT EXISTS`; later versions may only add.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;

pub(crate) const INDEX_FILE: &str = "index.sqlite3";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS session_links (
    broker_db        TEXT NOT NULL,
    session_id       INTEGER NOT NULL,
    agent_session_id TEXT NOT NULL DEFAULT '',
    transcript_path  TEXT NOT NULL DEFAULT '',
    source           TEXT NOT NULL,
    recorded_at      INTEGER NOT NULL,
    PRIMARY KEY (broker_db, session_id, agent_session_id, transcript_path)
);
CREATE INDEX IF NOT EXISTS session_links_by_agent ON session_links (agent_session_id);
CREATE INDEX IF NOT EXISTS session_links_by_path ON session_links (transcript_path);
CREATE TABLE IF NOT EXISTS backfill_cursors (
    broker_db     TEXT PRIMARY KEY,
    last_event_id INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS archives (
    original_path    TEXT PRIMARY KEY,
    archive_path     TEXT NOT NULL,
    provider         TEXT NOT NULL,
    agent_session_id TEXT,
    bytes_in         INTEGER NOT NULL,
    bytes_out        INTEGER NOT NULL,
    sha256           TEXT NOT NULL,
    archived_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS archives_by_agent ON archives (agent_session_id);
CREATE TABLE IF NOT EXISTS conflicts (
    original_path   TEXT PRIMARY KEY,
    attempts        INTEGER NOT NULL,
    first_seen      INTEGER NOT NULL,
    next_attempt_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS restore_intents (
    original_path TEXT PRIMARY KEY,
    archive_path  TEXT NOT NULL,
    started_at    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS counters (
    name  TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
";

/// One broker session linked to an agent session or transcript file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Link {
    pub broker_db: PathBuf,
    pub session_id: i64,
    pub agent_session_id: Option<String>,
    pub transcript_path: Option<PathBuf>,
    /// `holder_command` or `holder_open_files`.
    pub source: &'static str,
}

/// One archived transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArchiveRow {
    pub original_path: PathBuf,
    pub archive_path: PathBuf,
    pub provider: String,
    pub agent_session_id: Option<String>,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub sha256: String,
    pub archived_at: i64,
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn sql(error: rusqlite::Error) -> String {
    crate::host_state::describe_host_state_sqlite(&error)
}

pub(crate) struct Index {
    conn: Connection,
}

impl Index {
    /// Open (creating when absent) the index under `host_dir`.
    pub fn open(host_dir: &Path) -> Result<Self, String> {
        // The index names transcripts: it is created 0600 in a 0700
        // directory before SQLite opens it (SQLite gives its journal the
        // database file's mode).
        super::ensure_private_directory(host_dir)
            .map_err(|error| format!("cannot create the transcript state directory: {error}"))?;
        let path = host_dir.join(INDEX_FILE);
        super::ensure_private_file(&path)
            .map_err(|error| format!("cannot create the transcript index: {error}"))?;
        let conn = Connection::open(&path).map_err(sql)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(sql)?;
        conn.execute_batch(SCHEMA).map_err(sql)?;
        Ok(Self { conn })
    }

    /// Open the index read-only; `None` when it does not exist yet.
    pub fn open_existing(host_dir: &Path) -> Result<Option<Self>, String> {
        let path = host_dir.join(INDEX_FILE);
        if !path.is_file() {
            return Ok(None);
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sql)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(sql)?;
        Ok(Some(Self { conn }))
    }

    pub fn add_link(&self, link: &Link, now_ms: i64) -> Result<bool, String> {
        let inserted = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO session_links
                 (broker_db, session_id, agent_session_id, transcript_path, source, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    text(&link.broker_db),
                    link.session_id,
                    link.agent_session_id.as_deref().unwrap_or(""),
                    link.transcript_path
                        .as_deref()
                        .map(text)
                        .unwrap_or_default(),
                    link.source,
                    now_ms,
                ],
            )
            .map_err(sql)?;
        Ok(inserted > 0)
    }

    /// Broker sessions linked to this agent session id or transcript path.
    pub fn links_for(
        &self,
        agent_session_id: Option<&str>,
        path: &Path,
    ) -> Result<Vec<(PathBuf, i64)>, String> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT DISTINCT broker_db, session_id FROM session_links
                 WHERE (?1 <> '' AND agent_session_id = ?1) OR transcript_path = ?2
                 ORDER BY broker_db, session_id",
            )
            .map_err(sql)?;
        let rows = statement
            .query_map(params![agent_session_id.unwrap_or(""), text(path)], |row| {
                Ok((
                    PathBuf::from(row.get::<_, String>(0)?),
                    row.get::<_, i64>(1)?,
                ))
            })
            .map_err(sql)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql)
    }

    /// Every broker database a link or backfill has named.
    pub fn known_broker_dbs(&self) -> Result<Vec<PathBuf>, String> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT broker_db FROM backfill_cursors
                 UNION SELECT broker_db FROM session_links ORDER BY 1",
            )
            .map_err(sql)?;
        let rows = statement
            .query_map([], |row| Ok(PathBuf::from(row.get::<_, String>(0)?)))
            .map_err(sql)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(sql)
    }

    pub fn backfill_cursor(&self, broker_db: &Path) -> Result<i64, String> {
        self.conn
            .query_row(
                "SELECT last_event_id FROM backfill_cursors WHERE broker_db = ?1",
                [text(broker_db)],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map(|cursor| cursor.unwrap_or(0))
            .map_err(sql)
    }

    pub fn set_backfill_cursor(&self, broker_db: &Path, last_event_id: i64) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO backfill_cursors (broker_db, last_event_id) VALUES (?1, ?2)
                 ON CONFLICT(broker_db) DO UPDATE SET last_event_id = excluded.last_event_id",
                params![text(broker_db), last_event_id],
            )
            .map(|_| ())
            .map_err(sql)
    }

    pub fn record_archive(&self, row: &ArchiveRow) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO archives
                 (original_path, archive_path, provider, agent_session_id,
                  bytes_in, bytes_out, sha256, archived_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(original_path) DO UPDATE SET
                   archive_path = excluded.archive_path,
                   provider = excluded.provider,
                   agent_session_id = excluded.agent_session_id,
                   bytes_in = excluded.bytes_in,
                   bytes_out = excluded.bytes_out,
                   sha256 = excluded.sha256,
                   archived_at = excluded.archived_at",
                params![
                    text(&row.original_path),
                    text(&row.archive_path),
                    row.provider,
                    row.agent_session_id,
                    row.bytes_in as i64,
                    row.bytes_out as i64,
                    row.sha256,
                    row.archived_at,
                ],
            )
            .map(|_| ())
            .map_err(sql)
    }

    pub fn remove_archive(&self, original_path: &Path) -> Result<(), String> {
        self.conn
            .execute(
                "DELETE FROM archives WHERE original_path = ?1",
                [text(original_path)],
            )
            .map(|_| ())
            .map_err(sql)
    }

    fn archive_rows(&self, filter: &str, key: &str) -> Result<Vec<ArchiveRow>, String> {
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT original_path, archive_path, provider, agent_session_id,
                        bytes_in, bytes_out, sha256, archived_at
                 FROM archives {filter} ORDER BY archived_at, original_path"
            ))
            .map_err(sql)?;
        let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<ArchiveRow> {
            Ok(ArchiveRow {
                original_path: PathBuf::from(row.get::<_, String>(0)?),
                archive_path: PathBuf::from(row.get::<_, String>(1)?),
                provider: row.get(2)?,
                agent_session_id: row.get(3)?,
                bytes_in: row.get::<_, i64>(4)?.max(0) as u64,
                bytes_out: row.get::<_, i64>(5)?.max(0) as u64,
                sha256: row.get(6)?,
                archived_at: row.get(7)?,
            })
        };
        let rows = if filter.is_empty() {
            statement.query_map([], map).map_err(sql)?
        } else {
            statement.query_map([key], map).map_err(sql)?
        };
        rows.collect::<Result<Vec<_>, _>>().map_err(sql)
    }

    /// Every archived transcript.
    pub fn archives(&self) -> Result<Vec<ArchiveRow>, String> {
        self.archive_rows("", "")
    }

    /// Archived transcripts matching an agent session id or either path.
    pub fn find_archives(&self, key: &str) -> Result<Vec<ArchiveRow>, String> {
        self.archive_rows(
            "WHERE agent_session_id = lower(?1) OR original_path = ?1 OR archive_path = ?1",
            key,
        )
    }

    /// The archive row for one original path.
    pub fn archive_for(&self, original_path: &Path) -> Result<Option<ArchiveRow>, String> {
        let key = text(original_path);
        Ok(self
            .archive_rows("WHERE original_path = ?1", &key)?
            .into_iter()
            .next())
    }

    /// Whether a transcript with a recorded destination conflict may be
    /// tried again at `now_ms` (true when none is recorded).
    pub fn conflict_due(&self, original_path: &Path, now_ms: i64) -> Result<bool, String> {
        let next: Option<i64> = self
            .conn
            .query_row(
                "SELECT next_attempt_at FROM conflicts WHERE original_path = ?1",
                [text(original_path)],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)?;
        Ok(next.is_none_or(|next| now_ms >= next))
    }

    /// Record a destination conflict; the next attempt waits 1, 2, 4 ... up
    /// to 32 days.
    pub fn record_conflict(&self, original_path: &Path, now_ms: i64) -> Result<(), String> {
        let attempts: i64 = self
            .conn
            .query_row(
                "SELECT attempts FROM conflicts WHERE original_path = ?1",
                [text(original_path)],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql)?
            .unwrap_or(0)
            + 1;
        let backoff = super::eligibility::DAY_MS << (attempts - 1).clamp(0, 5);
        self.conn
            .execute(
                "INSERT INTO conflicts (original_path, attempts, first_seen, next_attempt_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(original_path) DO UPDATE SET
                   attempts = excluded.attempts,
                   next_attempt_at = excluded.next_attempt_at",
                params![text(original_path), attempts, now_ms, now_ms + backoff],
            )
            .map(|_| ())
            .map_err(sql)
    }

    pub fn clear_conflict(&self, original_path: &Path) -> Result<(), String> {
        self.conn
            .execute(
                "DELETE FROM conflicts WHERE original_path = ?1",
                [text(original_path)],
            )
            .map(|_| ())
            .map_err(sql)
    }

    /// Record that a restore of `original_path` is about to publish; a crash
    /// before [`Index::clear_restore_intent`] leaves it as evidence.
    pub fn record_restore_intent(
        &self,
        original_path: &Path,
        archive_path: &Path,
        now_ms: i64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO restore_intents (original_path, archive_path, started_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(original_path) DO UPDATE SET
                   archive_path = excluded.archive_path,
                   started_at = excluded.started_at",
                params![text(original_path), text(archive_path), now_ms],
            )
            .map(|_| ())
            .map_err(sql)
    }

    /// Restores that recorded an intent and have not finished their cleanup.
    pub fn pending_restore_intents(&self) -> Result<u64, String> {
        self.conn
            .query_row("SELECT COUNT(*) FROM restore_intents", [], |row| {
                row.get::<_, i64>(0)
            })
            .map(|count| count.max(0) as u64)
            .map_err(sql)
    }

    pub fn clear_restore_intent(&self, original_path: &Path) -> Result<(), String> {
        self.conn
            .execute(
                "DELETE FROM restore_intents WHERE original_path = ?1",
                [text(original_path)],
            )
            .map(|_| ())
            .map_err(sql)
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(sql)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
            .map_err(sql)
    }

    pub fn add_counters(&self, deltas: &BTreeMap<String, u64>) -> Result<(), String> {
        for (name, delta) in deltas.iter().filter(|(_, delta)| **delta > 0) {
            self.conn
                .execute(
                    "INSERT INTO counters (name, value) VALUES (?1, ?2)
                     ON CONFLICT(name) DO UPDATE SET value = value + excluded.value",
                    params![name, *delta as i64],
                )
                .map_err(sql)?;
        }
        Ok(())
    }

    pub fn counters(&self) -> Result<BTreeMap<String, u64>, String> {
        let mut statement = self
            .conn
            .prepare("SELECT name, value FROM counters ORDER BY name")
            .map_err(sql)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                ))
            })
            .map_err(sql)?;
        rows.collect::<Result<BTreeMap<_, _>, _>>().map_err(sql)
    }
}
