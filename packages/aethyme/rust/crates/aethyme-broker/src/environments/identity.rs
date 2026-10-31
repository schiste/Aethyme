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
    /// The recorded pid is running with the recorded start time.
    Alive,
    /// Neither the recorded process nor any member of its group exists.
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
        match self.observe_process() {
            Observation::Alive => {
                // SAFETY: getpgid has no memory-safety preconditions.
                let pgid = unsafe { libc::getpgid(self.pid) };
                if pgid == self.pgid {
                    Observation::Alive
                } else {
                    Observation::Unproven
                }
            }
            Observation::Unproven => Observation::Unproven,
            Observation::Gone if group_exists(self.pgid) => {
                // The leader exited but members of a group with that id
                // remain. A new process cannot reuse a pgid while any member
                // lives, but the leader's start time is gone, so ownership
                // can no longer be proven from here.
                Observation::Unproven
            }
            Observation::Gone => Observation::Gone,
        }
    }

    /// Observe only the recorded PID and start time. Busy `env exec` markers
    /// intentionally use this narrower proof because their command shares
    /// the invoking process group, which can outlive the command.
    pub(crate) fn observe_process(&self) -> Observation {
        let observed_start = crate::gates::process_start_time(self.pid);
        let presence = if observed_start.is_none() {
            process_presence(self.pid)
        } else {
            ProcessPresence::Unknown
        };
        observe_pid_start_time(self.start_time, observed_start, presence)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessPresence {
    Present,
    Absent,
    Unknown,
}

fn observe_pid_start_time(
    recorded_start: u64,
    observed_start: Option<u64>,
    presence: ProcessPresence,
) -> Observation {
    match observed_start {
        Some(start) if start == recorded_start => Observation::Alive,
        Some(_) => Observation::Unproven,
        None => match presence {
            ProcessPresence::Absent => Observation::Gone,
            ProcessPresence::Present | ProcessPresence::Unknown => Observation::Unproven,
        },
    }
}

fn presence_from_kill(result: i32, errno: Option<i32>) -> ProcessPresence {
    if result == 0 || errno == Some(libc::EPERM) {
        ProcessPresence::Present
    } else if errno == Some(libc::ESRCH) {
        ProcessPresence::Absent
    } else {
        ProcessPresence::Unknown
    }
}

fn process_presence(pid: i32) -> ProcessPresence {
    if pid <= 0 {
        return ProcessPresence::Unknown;
    }
    // SAFETY: signal 0 performs only the existence and permission check.
    let result = unsafe { libc::kill(pid, 0) };
    presence_from_kill(result, std::io::Error::last_os_error().raw_os_error())
}

pub(crate) fn group_exists(pgid: i32) -> bool {
    if pgid <= 0 {
        return false;
    }
    // SAFETY: signal 0 performs only the existence and permission check.
    let result = unsafe { libc::killpg(pgid, 0) };
    presence_from_kill(result, std::io::Error::last_os_error().raw_os_error())
        != ProcessPresence::Absent
}

#[cfg(test)]
mod tests {
    use super::{Observation, ProcessPresence, observe_pid_start_time, presence_from_kill};

    #[test]
    fn unavailable_or_ambiguous_process_probes_are_not_absence_proof() {
        assert_eq!(
            observe_pid_start_time(42, None, ProcessPresence::Unknown),
            Observation::Unproven
        );
        assert_eq!(
            observe_pid_start_time(42, None, ProcessPresence::Present),
            Observation::Unproven
        );
        assert_eq!(
            observe_pid_start_time(42, Some(43), ProcessPresence::Absent),
            Observation::Unproven
        );
        assert_eq!(
            observe_pid_start_time(42, None, ProcessPresence::Absent),
            Observation::Gone
        );
    }

    #[test]
    fn only_esrch_is_process_absence_proof() {
        assert_eq!(
            presence_from_kill(-1, Some(libc::ESRCH)),
            ProcessPresence::Absent
        );
        assert_eq!(
            presence_from_kill(-1, Some(libc::EPERM)),
            ProcessPresence::Present
        );
        assert_eq!(
            presence_from_kill(-1, Some(libc::EACCES)),
            ProcessPresence::Unknown
        );
    }
}
