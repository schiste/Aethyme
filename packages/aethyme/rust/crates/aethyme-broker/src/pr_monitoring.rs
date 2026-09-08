//! Per-session opt-in for pull request monitoring.
//!
//! Monitoring is off until a session asks for it. A scheduled poller that
//! watched every PR a fleet opens would make network calls nobody requested and
//! deliver interruptions to agents that never asked to be interrupted, so the
//! decision belongs to the session doing the work.
//!
//! Activation is a marker file under `.aethyme/run/`, which is gitignored
//! runtime state the broker already owns. Deliberately not a schema column:
//! this is per-session, ephemeral, and recreatable, and a storage migration is
//! a fleet-wide lockout risk for a flag whose loss costs one command.

use std::path::{Path, PathBuf};

fn monitoring_dir(main_root: &Path) -> PathBuf {
    main_root.join(".aethyme/run/pr-monitoring")
}

fn marker(main_root: &Path, session_id: i64) -> PathBuf {
    monitoring_dir(main_root).join(session_id.to_string())
}

/// Turn monitoring on for a session. Idempotent.
pub fn activate(main_root: &Path, session_id: i64) -> std::io::Result<()> {
    let dir = monitoring_dir(main_root);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(marker(main_root, session_id), b"active\n")
}

/// Turn it off. Idempotent: deactivating an inactive session is not an error,
/// so a cleanup path never has to check first.
pub fn deactivate(main_root: &Path, session_id: i64) -> std::io::Result<()> {
    match std::fs::remove_file(marker(main_root, session_id)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Whether this session asked to be told about its pull requests.
pub fn is_active(main_root: &Path, session_id: i64) -> bool {
    marker(main_root, session_id).is_file()
}

/// Sessions currently opted in, ascending.
pub fn active_sessions(main_root: &Path) -> Vec<i64> {
    let Ok(entries) = std::fs::read_dir(monitoring_dir(main_root)) else {
        return Vec::new();
    };
    let mut sessions: Vec<i64> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<i64>().ok())
        .collect();
    sessions.sort_unstable();
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_is_not_monitored_until_it_asks() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_active(tmp.path(), 1));
        assert!(active_sessions(tmp.path()).is_empty());
    }

    #[test]
    fn activation_round_trips_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        activate(tmp.path(), 7).unwrap();
        activate(tmp.path(), 7).unwrap();
        assert!(is_active(tmp.path(), 7));
        assert_eq!(active_sessions(tmp.path()), vec![7]);
    }

    /// A cleanup path must not have to check first.
    #[test]
    fn deactivating_a_session_that_never_opted_in_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        deactivate(tmp.path(), 9).unwrap();
        assert!(!is_active(tmp.path(), 9));
    }

    #[test]
    fn deactivation_leaves_other_sessions_alone() {
        let tmp = tempfile::tempdir().unwrap();
        activate(tmp.path(), 1).unwrap();
        activate(tmp.path(), 2).unwrap();
        deactivate(tmp.path(), 1).unwrap();
        assert_eq!(active_sessions(tmp.path()), vec![2]);
    }

    /// The directory holds one file per session; anything else is ignored
    /// rather than parsed into a bogus session id.
    #[test]
    fn unrelated_files_do_not_become_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        activate(tmp.path(), 3).unwrap();
        std::fs::write(monitoring_dir(tmp.path()).join("README"), b"x").unwrap();
        assert_eq!(active_sessions(tmp.path()), vec![3]);
    }
}
