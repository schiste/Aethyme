//! Who listens on a loopback port: an HTTP MCP server counts as ready only
//! when every TCP listener on its port belongs to its own process group and
//! is bound to `127.0.0.1`. "Something accepts connections" is never
//! enough: another local process could have taken the port first and would
//! then receive the agents' traffic.
//!
//! - Linux: `/proc/net/tcp` and `/proc/net/tcp6` list the listeners and
//!   their socket inodes; `/proc/<pid>/fd` of the group's processes tells
//!   which inodes are the group's.
//! - macOS: `netstat -anv -p tcp` (C locale, 2 s deadline), which lists
//!   every user's TCP sockets with the owning pid; each listener's pid is
//!   mapped to its group with `getpgid`. Output whose pid column cannot be
//!   found or read is unverifiable, never "nobody listens".
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

/// `(bound to 127.0.0.1 over IPv4, pid)` of each TCP listener on `port` in
/// `netstat -anv -p tcp` output. `None` when the output has no header with a
/// `pid` (or `process:pid`) column or a listener's pid cannot be read: the
/// full set of listeners is then unknown.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn parse_netstat(text: &str, port: u16) -> Option<Vec<(bool, u32)>> {
    let suffix = format!(".{port}");
    let loopback = format!("127.0.0.1{suffix}");
    let mut pid_column: Option<usize> = None;
    let mut found = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(first) = fields.first() else {
            continue;
        };
        if *first == "Proto" {
            // "Local Address" and "Foreign Address" are two words each.
            let mut columns: Vec<&str> = Vec::new();
            let mut index = 0;
            while index < fields.len() {
                let word = fields[index];
                if matches!(word, "Local" | "Foreign") && fields.get(index + 1) == Some(&"Address")
                {
                    columns.push("address");
                    index += 2;
                } else {
                    columns.push(word);
                    index += 1;
                }
            }
            pid_column = columns
                .iter()
                .position(|column| matches!(*column, "pid" | "process:pid"));
            continue;
        }
        if !first.starts_with("tcp") {
            continue;
        }
        let column = pid_column?;
        // Proto Recv-Q Send-Q Local Foreign (state) ...
        if fields.get(5) != Some(&"LISTEN") || !fields.get(3)?.ends_with(&suffix) {
            continue;
        }
        let pid = fields.get(column)?.rsplit(':').next()?.parse().ok()?;
        found.push((*first == "tcp4" && fields[3] == loopback, pid));
    }
    pid_column.map(|_| found)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{Listener, Listeners, judge, parse_netstat};

    const NETSTAT: &str = "/usr/sbin/netstat";
    const TOOL_TIMEOUT: Duration = Duration::from_secs(2);

    pub(super) fn check(port: u16, group: u32) -> Listeners {
        let spawned = Command::new(NETSTAT)
            .args(["-anv", "-p", "tcp"])
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else {
            return Listeners::Unverifiable("netstat_missing".into());
        };
        // Read while it runs: a busy host's table outgrows a pipe buffer.
        let Some(mut stdout) = child.stdout.take() else {
            let _killed = child.kill();
            let _reaped = child.wait();
            return Listeners::Unverifiable("netstat_output".into());
        };
        let reader = thread::spawn(move || {
            let mut text = String::new();
            stdout.read_to_string(&mut text).map(|_| text)
        });
        let deadline = Instant::now() + TOOL_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                _ => {
                    let _killed = child.kill();
                    let _reaped = child.wait();
                    let _drained = reader.join();
                    return Listeners::Unverifiable("netstat_timeout".into());
                }
            }
        };
        let text = match reader.join() {
            Ok(Ok(text)) if status.success() => text,
            _ => return Listeners::Unverifiable("netstat_failed".into()),
        };
        let Some(found) = parse_netstat(&text, port) else {
            return Listeners::Unverifiable("netstat_format".into());
        };
        let listeners: Vec<Listener> = found
            .into_iter()
            .map(|(loopback, pid)| {
                let pgid = libc::pid_t::try_from(pid)
                    // SAFETY: getpgid takes no pointers; it only reads a group id.
                    .map(|pid| unsafe { libc::getpgid(pid) })
                    .unwrap_or(-1);
                Listener {
                    loopback,
                    ours: u32::try_from(pgid).ok() == Some(group),
                }
            })
            .collect();
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

    #[test]
    fn netstat_listeners_are_read_with_their_pid_in_either_layout() {
        // Older layout: a `pid` column.
        let older = "Active Internet connections (including servers)
Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)      rhiwat  shiwat    pid   epid state  options           gencnt    flags   flags1 usscnt rtncnt fltrs
tcp4       0      0  127.0.0.1.47300        *.*                    LISTEN       131072  131072   4242      0 0x0100 0x00000006 0x000000000001a2b3 0x00000000 0x00000800      0      0 000001
tcp46      0      0  *.47300                *.*                    LISTEN       131072  131072   4343      0 0x0100 0x00000006 0x000000000001a2b4 0x00000000 0x00000800      0      0 000001
tcp4       0      0  127.0.0.1.47300        127.0.0.1.50000        ESTABLISHED  408241  146988   4242      0 0x0102 0x00000008 0x000000000001a2b5 0x00000000 0x00000800      1      0 000001
tcp4       0      0  127.0.0.1.147300       *.*                    LISTEN       131072  131072   4444      0 0x0100 0x00000006 0x000000000001a2b6 0x00000000 0x00000800      0      0 000001
udp4       0      0  *.47300                *.*                                 786896    9216   4545      0 0x0000 0x00000000 0x000000000001a2b7 0x00000000 0x00000800      0      0 000001
";
        assert_eq!(
            parse_netstat(older, 47300),
            Some(vec![(true, 4242), (false, 4343)])
        );
        // Newer layout: `process:pid`, with byte counters before it.
        let newer = "Active Internet connections (including servers)
Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)          rxbytes      txbytes  rhiwat  shiwat    process:pid    state  options           gencnt    flags   flags1 usscnt rtncnt fltrs
tcp6       0      0  ::1.47300              *.*                    LISTEN                 0            0  131072  131072   example:5151  00102 00000006 000000000001b2c3 00000000 00000800      0      0 000001
tcp4       0      0  127.0.0.1.47300        *.*                    LISTEN                 0            0  131072  131072   example:5150  00102 00000006 000000000001b2c4 00000000 00000800      0      0 000001
";
        assert_eq!(
            parse_netstat(newer, 47300),
            Some(vec![(false, 5151), (true, 5150)])
        );
        assert_eq!(parse_netstat(newer, 47301), Some(Vec::new()));
        // No pid column, or an unreadable pid: the set is unknown.
        let no_pid = "Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)
tcp4       0      0  127.0.0.1.47300        *.*                    LISTEN
";
        assert_eq!(parse_netstat(no_pid, 47300), None);
        let bad_pid = older.replace("   4242      0 0x0100", "   ????      0 0x0100");
        assert_eq!(parse_netstat(&bad_pid, 47300), None);
        assert_eq!(parse_netstat("", 47300), None);
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
