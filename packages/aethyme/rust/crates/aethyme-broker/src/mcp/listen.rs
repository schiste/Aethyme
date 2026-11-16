//! Who listens on a loopback port: an HTTP MCP server counts as ready only
//! when every TCP listener on its port belongs to its own process group and
//! is bound to `127.0.0.1`. "Something accepts connections" is never
//! enough: another local process could have taken the port first and would
//! then receive the agents' traffic.
//!
//! - Linux: `/proc/net/tcp` and `/proc/net/tcp6` list the listeners and
//!   their socket inodes; `/proc/<pid>/fd` of the group's processes tells
//!   which inodes are the group's.
//! - macOS: `lsof -nP -iTCP:<port> -sTCP:LISTEN -Fpn` (C locale, 2 s
//!   deadline), each listener's pid mapped to its group with `getpgid`.
//!   `lsof` run by a normal user may not list another user's sockets, so a
//!   squatter owned by another user could go unseen; the pre-spawn test
//!   bind still catches a port that was already taken.
//! - Anywhere else, or when the tool is missing or too slow: unverifiable,
//!   and the server is not used.

/// The verdict on one port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listeners {
    /// Every listener is the group's, on `127.0.0.1`.
    Ours,
    /// Nobody listens yet.
    None,
    /// A listener on this port belongs to another process.
    NotOurs,
    /// The group listens on something other than `127.0.0.1`.
    NotLoopback,
    /// Ownership could not be determined here.
    Unverifiable(String),
}

impl Listeners {
    /// The reason recorded when the server cannot be used.
    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Ours | Self::None => None,
            Self::NotOurs => Some("listener_not_ours".into()),
            Self::NotLoopback => Some("not_loopback".into()),
            Self::Unverifiable(why) => Some(format!("listener_unverifiable:{why}")),
        }
    }
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
/// One listener: whether it is bound to IPv4 loopback and whether the group
/// owns it.
struct Listener {
    loopback: bool,
    ours: bool,
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn judge(listeners: &[Listener]) -> Listeners {
    if listeners.is_empty() {
        Listeners::None
    } else if listeners.iter().any(|listener| !listener.ours) {
        Listeners::NotOurs
    } else if listeners.iter().any(|listener| !listener.loopback) {
        Listeners::NotLoopback
    } else {
        Listeners::Ours
    }
}

/// Judge the TCP listeners on `port` against process group `group`.
pub fn check(port: u16, group: u32) -> Listeners {
    #[cfg(target_os = "linux")]
    {
        linux::check(port, group)
    }
    #[cfg(target_os = "macos")]
    {
        macos::check(port, group)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _unused = (port, group);
        Listeners::Unverifiable("unsupported_platform".into())
    }
}

/// `(local address hex, inode)` of each listening socket on `port` in one
/// `/proc/net/tcp*` table.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn parse_proc_net(table: &str, port: u16) -> Vec<(String, u64)> {
    let wanted = format!("{port:04X}");
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let (address, local_port) = fields.get(1)?.rsplit_once(':')?;
            // 0A: TCP_LISTEN.
            (local_port.eq_ignore_ascii_case(&wanted) && *fields.get(3)? == "0A")
                .then(|| Some((address.to_ascii_uppercase(), fields.get(9)?.parse().ok()?)))
                .flatten()
        })
        .collect()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashSet;
    use std::fs;

    use super::{Listener, Listeners, judge, parse_proc_net};

    /// 127.0.0.1 as `/proc/net/tcp` prints it (host byte order, little endian).
    const LOOPBACK_V4: &str = "0100007F";

    /// Socket inodes held open by the processes of `group`.
    fn group_sockets(group: u32) -> HashSet<u64> {
        let mut sockets = HashSet::new();
        let Ok(processes) = fs::read_dir("/proc") else {
            return sockets;
        };
        for process in processes.flatten() {
            let name = process.file_name();
            let Some(pid) = name
                .to_str()
                .filter(|name| name.bytes().all(|b| b.is_ascii_digit()))
            else {
                continue;
            };
            let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            // After the `(comm)` field: state, ppid, pgrp.
            let pgrp = stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(2))
                .and_then(|field| field.parse::<u32>().ok());
            if pgrp != Some(group) {
                continue;
            }
            let Ok(descriptors) = fs::read_dir(format!("/proc/{pid}/fd")) else {
                continue;
            };
            for descriptor in descriptors.flatten() {
                let Ok(target) = fs::read_link(descriptor.path()) else {
                    continue;
                };
                if let Some(inode) = target
                    .to_str()
                    .and_then(|target| target.strip_prefix("socket:["))
                    .and_then(|rest| rest.strip_suffix(']'))
                    .and_then(|inode| inode.parse().ok())
                {
                    sockets.insert(inode);
                }
            }
        }
        sockets
    }

    pub(super) fn check(port: u16, group: u32) -> Listeners {
        let Ok(v4) = fs::read_to_string("/proc/net/tcp") else {
            return Listeners::Unverifiable("proc_net_unreadable".into());
        };
        // No IPv6 table means no IPv6 listeners.
        let v6 = fs::read_to_string("/proc/net/tcp6").unwrap_or_default();
        let found: Vec<(bool, u64)> = parse_proc_net(&v4, port)
            .into_iter()
            .map(|(address, inode)| (address == LOOPBACK_V4, inode))
            .chain(
                parse_proc_net(&v6, port)
                    .into_iter()
                    .map(|(_, inode)| (false, inode)),
            )
            .collect();
        if found.is_empty() {
            return Listeners::None;
        }
        let ours = group_sockets(group);
        let listeners: Vec<Listener> = found
            .into_iter()
            .map(|(loopback, inode)| Listener {
                loopback,
                ours: ours.contains(&inode),
            })
            .collect();
        judge(&listeners)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{Listener, Listeners, judge};

    const LSOF: &str = "/usr/sbin/lsof";
    const TOOL_TIMEOUT: Duration = Duration::from_secs(2);

    pub(super) fn check(port: u16, group: u32) -> Listeners {
        let spawned = Command::new(LSOF)
            .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpn"])
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else {
            return Listeners::Unverifiable("lsof_missing".into());
        };
        let deadline = Instant::now() + TOOL_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                _ => {
                    let _killed = child.kill();
                    let _reaped = child.wait();
                    return Listeners::Unverifiable("lsof_timeout".into());
                }
            }
        };
        let mut text = String::new();
        if let Some(stdout) = child.stdout.as_mut()
            && stdout.read_to_string(&mut text).is_err()
        {
            return Listeners::Unverifiable("lsof_output".into());
        }
        // lsof exits 1 when it finds nothing.
        if text.trim().is_empty() {
            return if status.success() || status.code() == Some(1) {
                Listeners::None
            } else {
                Listeners::Unverifiable("lsof_failed".into())
            };
        }
        let loopback = format!("127.0.0.1:{port}");
        let mut listeners = Vec::new();
        let mut owner: Option<i32> = None;
        for line in text.lines() {
            if let Some(pid) = line.strip_prefix('p') {
                owner = pid.parse().ok();
            } else if let Some(name) = line.strip_prefix('n') {
                let Some(pid) = owner else {
                    return Listeners::Unverifiable("lsof_output".into());
                };
                // SAFETY: getpgid takes no pointers; it only reads a group id.
                let pgid = unsafe { libc::getpgid(pid) };
                listeners.push(Listener {
                    loopback: name == loopback,
                    ours: u32::try_from(pgid).ok() == Some(group),
                });
            }
        }
        judge(&listeners)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_net_rows_are_read_by_port_and_state() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:B8AC 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1 0000000000000000 100 0 0 10 0
   1: 00000000:B8AC 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4343 1 0000000000000000 100 0 0 10 0
   2: 0100007F:B8AC 0100007F:9C40 01 00000000:00000000 00:00000000 00000000  1000        0 4444 1 0000000000000000 20 4 30 10 -1
   3: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4545 1 0000000000000000 100 0 0 10 0
";
        assert_eq!(
            parse_proc_net(table, 0xB8AC),
            vec![
                ("0100007F".to_string(), 4242),
                ("00000000".to_string(), 4343)
            ]
        );
    }

    #[test]
    fn a_verdict_needs_every_listener_ours_and_on_loopback() {
        let listener = |loopback, ours| Listener { loopback, ours };
        assert_eq!(judge(&[]), Listeners::None);
        assert_eq!(judge(&[listener(true, true)]), Listeners::Ours);
        assert_eq!(
            judge(&[listener(true, true), listener(true, false)]),
            Listeners::NotOurs
        );
        assert_eq!(
            judge(&[listener(true, true), listener(false, true)]),
            Listeners::NotLoopback
        );
        assert_eq!(
            Listeners::NotOurs.reason().as_deref(),
            Some("listener_not_ours")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_listener_of_another_group_is_not_ours() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // SAFETY: getpgrp takes no arguments.
        let own = u32::try_from(unsafe { libc::getpgrp() }).unwrap();
        assert_eq!(check(port, own), Listeners::Ours);
        assert_eq!(check(port, own.wrapping_add(1_000_000)), Listeners::NotOurs);
        drop(listener);
        assert_eq!(check(port, own), Listeners::None);
        let wide = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let wide_port = wide.local_addr().unwrap().port();
        assert_eq!(check(wide_port, own), Listeners::NotLoopback);
    }
}
