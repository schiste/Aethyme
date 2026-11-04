//! Cross-process wakeups for the durable SQLite event log.
//!
//! SQLite writes its WAL and journal as sibling files, so watch the database
//! directory rather than one replaceable file. The notification is only a
//! hint: callers always replay rows from their durable event-id cursor.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

pub(super) struct EventWakeup {
    // Keep the watcher alive for as long as its receiver is used.
    _watcher: notify::RecommendedWatcher,
    events: Receiver<notify::Result<notify::Event>>,
}

impl EventWakeup {
    pub(super) fn watch_database(database_path: &Path) -> Result<Self, String> {
        let parent = database_path.parent().ok_or_else(|| {
            format!(
                "broker database path has no parent to watch: {}",
                database_path.display()
            )
        })?;
        let (sender, events) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })
        .map_err(|error| format!("cannot watch broker database directory: {error}"))?;
        watcher
            .watch(parent, RecursiveMode::NonRecursive)
            .map_err(|error| format!("cannot watch broker database directory: {error}"))?;
        Ok(Self {
            _watcher: watcher,
            events,
        })
    }

    pub(super) fn wait(&self) -> Result<(), String> {
        self.receive(None).map(|_| ())
    }

    pub(super) fn wait_timeout(&self, timeout: Duration) -> Result<bool, String> {
        self.receive(Some(timeout))
    }

    fn receive(&self, timeout: Option<Duration>) -> Result<bool, String> {
        let event = match timeout {
            Some(timeout) => match self.events.recv_timeout(timeout) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => return Ok(false),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("broker database notification channel closed".into());
                }
            },
            None => self
                .events
                .recv()
                .map_err(|_| "broker database notification channel closed".to_string())?,
        };
        event
            .map(|_| true)
            .map_err(|error| format!("broker database notification failed: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::EventWakeup;
    use std::fs;
    use std::time::Duration;

    #[test]
    fn database_sidecar_change_wakes_the_follower() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("broker.db");
        fs::write(&database, b"database").unwrap();
        let wakeup = EventWakeup::watch_database(&database).unwrap();

        // SQLite's WAL commit file sits beside the database and can be
        // created or replaced independently of the main database file.
        fs::write(directory.path().join("broker.db-wal"), b"commit").unwrap();

        assert!(wakeup.wait_timeout(Duration::from_secs(5)).unwrap());
    }
}
