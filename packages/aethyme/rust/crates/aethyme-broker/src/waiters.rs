//! Who is waiting on which broker lock or lease right now (#494).
//!
//! A broker command that blocks on a contested lock already says so on its
//! own stderr. Another terminal saw nothing: `broker status` could not tell a
//! session queued behind a lock from one that hung. Each contended wait now
//! keeps one small record under `.aethyme/run/waits/` for as long as it waits,
//! and `broker status` lists them.
//!
//! Records are files, not database rows, for the same reasons as
//! `submit_progress`: they describe a process that is running right now, they
//! must be readable while the broker database is the contested resource, and
//! a waiter killed mid-wait leaves a record whose PID is dead -- reported as
//! `alive: false` -- rather than a row a migration has to understand.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::clock::epoch_ms;

/// What a waiter is blocked on.
pub(crate) const WAIT_COORDINATED_WRITE_LOCK: &str = "coordinated_write_lock";
pub(crate) const WAIT_GATE_OWNER_LOCK: &str = "gate_owner_lock";
pub(crate) const WAIT_LEASE: &str = "lease";

fn waits_dir(main_root: &Path) -> PathBuf {
    main_root.join(".aethyme/run/waits")
}

/// One wait as the waiting process records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitRecord {
    pub pid: i64,
    /// The waiting session, when the wait runs on behalf of one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<i64>,
    /// `coordinated_write_lock`, `gate_owner_lock` or `lease`.
    pub kind: String,
    /// The lock or lease waited for: a repository, a gate and scope, a path.
    pub resource: String,
    /// The holder as last observed, e.g. `session 812 for 3m10s`.
    pub holder: String,
    pub started_at_ms: i64,
}

/// One wait as `broker status` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CurrentWaiter {
    pub pid: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<i64>,
    pub kind: String,
    pub resource: String,
    pub holder: String,
    pub waited_ms: i64,
    /// False when the waiting process is gone: a killed waiter's record.
    pub alive: bool,
}

/// Every wait recorded in this repository, longest first.
pub fn current_waiters(main_root: &Path, now_ms: i64) -> Vec<CurrentWaiter> {
    current_with(main_root, now_ms, crate::broker::pid_alive)
}

fn current_with(main_root: &Path, now_ms: i64, alive: impl Fn(i64) -> bool) -> Vec<CurrentWaiter> {
    let Ok(entries) = std::fs::read_dir(waits_dir(main_root)) else {
        return Vec::new();
    };
    let mut waiters = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| std::fs::read(entry.path()).ok())
        .filter_map(|bytes| serde_json::from_slice::<WaitRecord>(&bytes).ok())
        .map(|record| CurrentWaiter {
            alive: alive(record.pid),
            waited_ms: now_ms.saturating_sub(record.started_at_ms).max(0),
            pid: record.pid,
            session_id: record.session_id,
            kind: record.kind,
            resource: record.resource,
            holder: record.holder,
        })
        .collect::<Vec<_>>();
    waiters.sort_by(|a, b| {
        b.waited_ms
            .cmp(&a.waited_ms)
            .then_with(|| a.pid.cmp(&b.pid))
    });
    waiters
}

/// `waiting for the coordinated write lock on schiste/aethyme: held by …`.
pub(crate) fn describe(waiter: &CurrentWaiter) -> String {
    let who = match waiter.session_id {
        Some(session) => format!("session {session}"),
        None => format!("process {}", waiter.pid),
    };
    let mut line = format!(
        "{who} waiting {} for {} {}: {}",
        crate::submit_progress::duration_label(waiter.waited_ms),
        kind_label(&waiter.kind),
        waiter.resource,
        waiter.holder
    );
    if !waiter.alive {
        line.push_str(" -- process gone");
    }
    line
}

fn kind_label(kind: &str) -> &str {
    match kind {
        WAIT_COORDINATED_WRITE_LOCK => "the coordinated write lock on",
        WAIT_GATE_OWNER_LOCK => "the gate owner lock",
        WAIT_LEASE => "the lease on",
        other => other,
    }
}

/// The record of one wait in this process. Dropping it removes the record,
/// so only a killed waiter leaves one behind.
pub(crate) struct WaitRegistration {
    path: PathBuf,
    record: WaitRecord,
}

static NEXT_WAIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl WaitRegistration {
    /// Record that this process has started waiting. Recording is advisory:
    /// a failure to write warns and the wait goes on unrecorded.
    pub(crate) fn start(
        main_root: &Path,
        session_id: Option<i64>,
        kind: &str,
        resource: &str,
        holder: &str,
    ) -> Self {
        let pid = std::process::id();
        let sequence = NEXT_WAIT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let registration = Self {
            path: waits_dir(main_root).join(format!("{pid}-{sequence}.json")),
            record: WaitRecord {
                pid: i64::from(pid),
                session_id,
                kind: kind.to_string(),
                resource: resource.to_string(),
                holder: holder.to_string(),
                started_at_ms: epoch_ms(),
            },
        };
        registration.persist();
        registration
    }

    /// The holder changed or aged; keep the record current.
    pub(crate) fn update_holder(&mut self, holder: &str) {
        if self.record.holder != holder {
            self.record.holder = holder.to_string();
            self.persist();
        }
    }

    fn persist(&self) {
        let Ok(bytes) = serde_json::to_vec(&self.record) else {
            return;
        };
        let target = &self.path;
        let written = target
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                crate::atomic_file::with_synced_temporary(target, &bytes, |temporary| {
                    std::fs::rename(temporary, target)
                })
            });
        crate::warn_unrecorded("record a lock wait", written);
    }
}

impl Drop for WaitRegistration {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("Warning: cannot remove lock wait record: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_is_listed_while_it_lasts_and_removed_when_it_ends() {
        let root = tempfile::tempdir().unwrap();
        let mut wait = WaitRegistration::start(
            root.path(),
            Some(812),
            WAIT_LEASE,
            "src/lib.rs",
            "held by session 700 (lease age 4m)",
        );
        let waiters = current_waiters(root.path(), epoch_ms());
        assert_eq!(waiters.len(), 1);
        assert_eq!(waiters[0].session_id, Some(812));
        assert_eq!(waiters[0].kind, WAIT_LEASE);
        assert_eq!(waiters[0].resource, "src/lib.rs");
        assert!(waiters[0].alive, "this process is the waiter");

        wait.update_holder("held by session 701 (lease age 1s)");
        let waiters = current_waiters(root.path(), epoch_ms());
        assert_eq!(waiters[0].holder, "held by session 701 (lease age 1s)");

        drop(wait);
        assert!(current_waiters(root.path(), epoch_ms()).is_empty());
    }

    #[test]
    fn a_killed_waiter_is_reported_as_gone_and_waits_sort_longest_first() {
        let root = tempfile::tempdir().unwrap();
        let dir = waits_dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        for (pid, started) in [(41, 9_000), (42, 1_000)] {
            let record = WaitRecord {
                pid,
                session_id: None,
                kind: WAIT_COORDINATED_WRITE_LOCK.into(),
                resource: "schiste/aethyme".into(),
                holder: "operation 7 (session 3)".into(),
                started_at_ms: started,
            };
            std::fs::write(
                dir.join(format!("{pid}-1.json")),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
        }
        std::fs::write(dir.join("junk.json"), b"not json").unwrap();
        let waiters = current_with(root.path(), 10_000, |pid| pid == 41);
        assert_eq!(
            waiters
                .iter()
                .map(|w| (w.pid, w.waited_ms, w.alive))
                .collect::<Vec<_>>(),
            vec![(42, 9_000, false), (41, 1_000, true)]
        );
        assert_eq!(
            describe(&waiters[0]),
            "process 42 waiting 9s for the coordinated write lock on schiste/aethyme: \
             operation 7 (session 3) -- process gone"
        );
    }
}
