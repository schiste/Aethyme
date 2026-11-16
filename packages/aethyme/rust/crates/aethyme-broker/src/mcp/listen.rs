//! Who listens on a loopback port: an HTTP MCP server counts as ready only
//! when every TCP listener on its port belongs to its own process group and
//! is bound to `127.0.0.1`. "Something accepts connections" is never
//! enough: another local process could have taken the port first and would
//! then receive the agents' traffic.
//!
//! - Linux: `/proc/net/tcp` and `/proc/net/tcp6` list the listeners and
//!   their socket inodes; `/proc/<pid>/fd` of the group's processes tells
//!   which inodes are the group's.
//! - macOS: two counts that must agree. `netstat -an -p tcp` (C locale, 2 s
//!   deadline) lists every user's TCP sockets; only its leading columns
//!   (protocol, queues, local and foreign address, state), which come
//!   before anything a process controls, are read, to count the listeners
//!   on the port and their addresses. `lsof -a -g <group> -nP
//!   -iTCP:<port> -sTCP:LISTEN -Fdn`, limited to the server's own process
//!   group, gives the listening sockets the group owns (deduplicated by
//!   socket, since forked workers share one). Ours only when both counts
//!   are equal and every listener is on 127.0.0.1; fewer owned than
//!   listening is `listener_not_ours`; anything unreadable or of an
//!   unexpected shape is unverifiable. No pid or process name is ever read
//!   from another user's socket entry, so a crafted process name cannot
//!   claim a listener. A process name with a line break could add a fake
//!   `LISTEN` line, which only raises the count: it fails closed.
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

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// One listener: whether it is bound to IPv4 loopback and whether the group
/// owns it.
struct Listener {
    loopback: bool,
    ours: bool,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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

/// For each TCP listener on `port` in `netstat -an -p tcp` output: whether
/// it is bound to 127.0.0.1 over IPv4. Only the leading columns are read
/// (`Proto Recv-Q Send-Q Local Foreign (state)`); `None` when any TCP line
/// does not have that shape, so the count is never a guess.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn netstat_listeners(text: &str, port: u16) -> Option<Vec<bool>> {
    let suffix = format!(".{port}");
    let loopback = format!("127.0.0.1{suffix}");
    let mut found = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(proto) = fields.next() else {
            continue;
        };
        if !proto.starts_with("tcp") {
            continue;
        }
        if !matches!(proto, "tcp4" | "tcp6" | "tcp46") {
            return None;
        }
        let (Some(receive), Some(send), Some(local), Some(_foreign), Some(state)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return None;
        };
        let counter = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
        if !counter(receive) || !counter(send) {
            return None;
        }
        if state == "LISTEN" && local.ends_with(&suffix) {
            found.push(proto == "tcp4" && local == loopback);
        }
    }
    Some(found)
}

/// The group's own listening sockets on `port` from `lsof -a -g <group> -nP
/// -iTCP:<port> -sTCP:LISTEN -Fdn` (fields `p`, `f`, `d`, `n`): whether each
/// distinct socket (by its `d` kernel address) is bound to 127.0.0.1.
/// `None` when a name comes without its socket address.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn lsof_owned_sockets(text: &str, port: u16) -> Option<Vec<bool>> {
    let loopback = format!("127.0.0.1:{port}");
    let mut sockets = std::collections::BTreeMap::new();
    let mut device: Option<&str> = None;
    for line in text.lines() {
        match line.split_at_checked(1) {
            Some(("p" | "f", _)) => device = None,
            Some(("d", value)) => device = Some(value),
            Some(("n", name)) => {
                let socket = device.take()?;
                sockets.insert(socket, name == loopback);
            }
            _ => {}
        }
    }
    Some(sockets.into_values().collect())
}

/// Why a listing is unverifiable when it shows nothing the group holds, or
/// nothing at all.
pub const LISTING_EMPTY: &str = "listener_listing_empty";

/// Combine the host-wide listeners on the port with the ones the group owns.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn reconcile(all: &[bool], owned: &[bool]) -> Listeners {
    if all.is_empty() && !owned.is_empty() {
        // The group holds a listener the host-wide listing does not show: the
        // listing is incomplete (a sandbox can make it print nothing).
        Listeners::Unverifiable(LISTING_EMPTY.into())
    } else if all.is_empty() {
        Listeners::None
    } else if owned.len() < all.len() {
        Listeners::NotOurs
    } else if owned.len() > all.len() {
        Listeners::Unverifiable("listener_count_mismatch".into())
    } else if all.iter().chain(owned).any(|loopback| !loopback) {
        Listeners::NotLoopback
    } else {
        Listeners::Ours
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{Listeners, lsof_owned_sockets, netstat_listeners, reconcile};

    const NETSTAT: &str = "/usr/sbin/netstat";
    const LSOF: &str = "/usr/sbin/lsof";
    const TOOL_TIMEOUT: Duration = Duration::from_secs(2);

    /// Run a tool under the C locale within [`TOOL_TIMEOUT`], reading its
    /// output while it runs; its exit status and output.
    fn run(program: &str, args: &[&str]) -> Result<(Option<i32>, String), String> {
        let mut child = Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| format!("{program}_missing"))?;
        let Some(mut stdout) = child.stdout.take() else {
            let _killed = child.kill();
            let _reaped = child.wait();
            return Err("tool_output".into());
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
                    return Err("tool_timeout".into());
                }
            }
        };
        match reader.join() {
            Ok(Ok(text)) => Ok((status.code(), text)),
            _ => Err("tool_output".into()),
        }
    }

    pub(super) fn check(port: u16, group: u32) -> Listeners {
        // Both always run: an empty listing must be told apart from a port
        // nobody listens on.
        let all = match run(NETSTAT, &["-an", "-p", "tcp"]) {
            // A host always has TCP sockets: an empty table is a listing the
            // caller may not see (an agent sandbox prints nothing, exit 0).
            Ok((Some(0), text)) if text.trim().is_empty() => {
                return Listeners::Unverifiable(LISTING_EMPTY.into());
            }
            Ok((Some(0), text)) => match netstat_listeners(&text, port) {
                Some(all) => all,
                None => return Listeners::Unverifiable("netstat_format".into()),
            },
            Ok(_) => return Listeners::Unverifiable("netstat_failed".into()),
            Err(reason) => return Listeners::Unverifiable(reason),
        };
        let selector = format!("-iTCP:{port}");
        let group_text = group.to_string();
        let owned = match run(
            LSOF,
            &[
                "-a",
                "-g",
                &group_text,
                "-nP",
                &selector,
                "-sTCP:LISTEN",
                "-Fdn",
            ],
        ) {
            // lsof exits 1 when it selects nothing.
            Ok((Some(0 | 1), text)) => match lsof_owned_sockets(&text, port) {
                Some(owned) => owned,
                None => return Listeners::Unverifiable("lsof_format".into()),
            },
            Ok(_) => return Listeners::Unverifiable("lsof_failed".into()),
            Err(reason) => return Listeners::Unverifiable(reason),
        };
        reconcile(&all, &owned)
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
    fn netstat_counts_listeners_from_the_leading_columns_only() {
        let table = "Active Internet connections (including servers)
Proto Recv-Q Send-Q  Local Address          Foreign Address        (state)      rhiwat  shiwat    pid   epid
tcp4       0      0  127.0.0.1.47300        *.*                    LISTEN       131072  131072   4242      0
tcp46      0      0  *.47300                *.*                    LISTEN       131072  131072   4343      0
tcp4       0      0  127.0.0.1.47300        127.0.0.1.50000        ESTABLISHED  408241  146988   4242      0
tcp4       0      0  127.0.0.1.147300       *.*                    LISTEN       131072  131072   4444      0
udp4       0      0  *.47300                *.*                                 786896    9216   4545      0
";
        assert_eq!(netstat_listeners(table, 47300), Some(vec![true, false]));
        assert_eq!(netstat_listeners(table, 47301), Some(Vec::new()));
        // Hostile trailing columns (a process name with spaces, colons and
        // a `name:1234` shape) change nothing: only the leading columns count.
        let hostile = "tcp4       0      0  127.0.0.1.47300        *.*                    LISTEN       0 0 131072 131072   evil name:1234 x:99 more:5150  00102
tcp6       0      0  ::1.47300              *.*                    LISTEN       0 0 131072 131072   a b c:4242
";
        assert_eq!(netstat_listeners(hostile, 47300), Some(vec![true, false]));
        // A line of an unexpected shape makes the count unknown.
        let truncated = "tcp4       0      0  127.0.0.1.47300\n";
        assert_eq!(netstat_listeners(truncated, 47300), None);
        let not_counters = "tcp4  x  0  127.0.0.1.47300  *.*  LISTEN\n";
        assert_eq!(netstat_listeners(not_counters, 47300), None);
        let odd_protocol = "tcpX  0  0  127.0.0.1.47300  *.*  LISTEN\n";
        assert_eq!(netstat_listeners(odd_protocol, 47300), None);
        assert_eq!(netstat_listeners("", 47300), Some(Vec::new()));
    }

    #[test]
    fn lsof_sockets_are_deduplicated_and_need_their_address() {
        // Two workers of the group share one listening socket.
        let shared =
            "p4242\nf5\nd0x1a2b3c\nn127.0.0.1:47300\np4243\nf5\nd0x1a2b3c\nn127.0.0.1:47300\n";
        assert_eq!(lsof_owned_sockets(shared, 47300), Some(vec![true]));
        let two = "p4242\nf5\nd0x1\nn127.0.0.1:47300\nf6\nd0x2\nn*:47300\n";
        assert_eq!(lsof_owned_sockets(two, 47300), Some(vec![true, false]));
        assert_eq!(lsof_owned_sockets("", 47300), Some(Vec::new()));
        let no_address = "p4242\nf5\nn127.0.0.1:47300\n";
        assert_eq!(lsof_owned_sockets(no_address, 47300), None);
    }

    #[test]
    fn listeners_are_ours_only_when_the_counts_agree() {
        assert_eq!(reconcile(&[], &[]), Listeners::None);
        assert_eq!(
            reconcile(&[], &[true]),
            Listeners::Unverifiable(LISTING_EMPTY.into())
        );
        assert_eq!(reconcile(&[true], &[true]), Listeners::Ours);
        assert_eq!(reconcile(&[true, true], &[true]), Listeners::NotOurs);
        assert_eq!(reconcile(&[true], &[]), Listeners::NotOurs);
        assert_eq!(
            reconcile(&[true], &[true, true]),
            Listeners::Unverifiable("listener_count_mismatch".into())
        );
        assert_eq!(reconcile(&[false], &[false]), Listeners::NotLoopback);
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
