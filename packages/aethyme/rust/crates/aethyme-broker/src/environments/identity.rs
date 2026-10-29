//! Process identity for things environments start (#1222).
//!
//! A pid alone is not an identity: once a process exits its pid is recycled,
//! and signalling the recycled pid hits an unrelated process. Identity here is
//! `(pid, pgid, start_time)`, captured right after spawn, and `observe` only
//! ever answers `Alive` when all three still match. Everything else is
//! `Gone` (provably absent) or `Unproven` (something answers, but nothing
//! proves it is ours). Callers signal only `Alive`; `Unproven` is quarantined
//! and reported, never signalled.
//!
//! This generalises `console_holder_identity`, which compares a holder's start
//! time with its lease's grant time.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: i32,
    pub pgid: i32,
    /// Opaque, comparable only on this host (`gates::process_start_time`).
    pub start_time: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Observation {
    /// The recorded leader is running with the recorded start time.
    Alive,
    /// Neither the leader nor any member of its group exists.
    Gone,
    /// Something exists at the recorded pid or group, but its identity does
    /// not match or cannot be read. Never signal it.
    Unproven,
}

impl ProcessIdentity {
    /// Capture the identity of a running process. `None` if it has already
    /// exited or the platform cannot report a start time.
    pub fn capture(pid: i32) -> Option<Self> {
        if pid <= 0 {
            return None;
        }
        let start_time = crate::gates::process_start_time(pid)?;
        // SAFETY: getpgid has no memory-safety preconditions.
        let pgid = unsafe { libc::getpgid(pid) };
        if pgid <= 0 {
            return None;
        }
        Some(Self {
            pid,
            pgid,
            start_time,
        })
    }

    pub fn observe(&self) -> Observation {
        match crate::gates::process_start_time(self.pid) {
            Some(start) if start == self.start_time => {
                // SAFETY: getpgid has no memory-safety preconditions.
                let pgid = unsafe { libc::getpgid(self.pid) };
                if pgid == self.pgid {
                    Observation::Alive
                } else {
                    Observation::Unproven
                }
            }
            Some(_) => Observation::Unproven,
            None if process_exists(self.pid) => Observation::Unproven,
            None if group_exists(self.pgid) => {
                // The leader exited but members of a group with that id
                // remain. A new process cannot reuse a pgid while any member
                // lives, but the leader's start time is gone, so ownership
                // can no longer be proven from here.
                Observation::Unproven
            }
            None => Observation::Gone,
        }
    }
}

fn process_exists(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 performs only the existence and permission check.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

pub(crate) fn group_exists(pgid: i32) -> bool {
    if pgid <= 0 {
        return false;
    }
    // SAFETY: signal 0 performs only the existence and permission check.
    let result = unsafe { libc::killpg(pgid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
