//! Which volumes foreground work is writing to right now.
//!
//! A dependency install or a gate is work an agent waits on. Unattended bulk
//! deletion on the same volume competes with it for the disk: measured on an
//! external volume, a 30-minute offline install used about 6 s of child CPU
//! while the checkout trash catch-up removed 17.7 GB beside it. The install
//! was starved for I/O, not CPU.
//!
//! Foreground work leaves one marker file per run in the host state
//! directory, named for the volume (device number) it writes to and its
//! process. Unattended deleters read the directory once per slice and yield
//! the disk while a live marker names a volume they delete on. A marker whose
//! process is gone counts for nothing and is removed by the next reader, so a
//! crashed run never pins a deleter in its yielding state.
//!
//! Everything here is advisory and best effort: a marker that cannot be
//! written only means the deleter does not yield.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// The marker directory beneath the host state directory.
const DIRECTORY: &str = "run/foreground-io";

/// At most this many markers are read per check; a directory that holds more
/// is treated as "foreground work is running" rather than walked.
const MAX_SCANNED_MARKERS: usize = 256;

/// One live foreground run's marker. Removed on drop.
#[derive(Debug)]
pub(crate) struct ForegroundIoMark {
    path: PathBuf,
}

impl ForegroundIoMark {
    /// Mark foreground work of `kind` (`preparation`, `gate`) writing on the
    /// volume holding `path`. `None` when there is no host state directory or
    /// the marker cannot be written.
    pub(crate) fn enter(kind: &str, path: &Path) -> Option<Self> {
        Self::enter_in(&marker_directory()?, kind, path)
    }

    pub(crate) fn enter_in(directory: &Path, kind: &str, path: &Path) -> Option<Self> {
        let device = device_of(path)?;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        std::fs::create_dir_all(directory).ok()?;
        let marker = directory.join(marker_name(device, std::process::id(), sequence));
        std::fs::write(&marker, kind).ok()?;
        crate::phase_telemetry::add("foreground_io_marked", std::time::Duration::ZERO);
        Some(Self { path: marker })
    }
}

impl Drop for ForegroundIoMark {
    fn drop(&mut self) {
        // Best effort: a marker left behind names a process that is gone,
        // which readers ignore and remove.
        let _ = std::fs::remove_file(&self.path);
    }
}

fn marker_name(device: u64, pid: u32, sequence: u64) -> String {
    format!("{device}-{pid}-{sequence}")
}

/// `(device, pid)` from a marker's file name.
fn parse_marker_name(name: &str) -> Option<(u64, u32)> {
    let mut parts = name.split('-');
    let device = parts.next()?.parse().ok()?;
    let pid = parts.next()?.parse().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    parts.next().is_none().then_some((device, pid))
}

/// Whether foreground work runs on any of `devices`. With no host state
/// directory nothing can be known and the answer is `false`.
pub(crate) fn active_on(devices: &[u64]) -> bool {
    marker_directory().is_some_and(|directory| active_on_in(&directory, devices, process_alive))
}

/// [`active_on`] for `directory`, with the liveness probe injected.
pub(crate) fn active_on_in(directory: &Path, devices: &[u64], alive: impl Fn(u32) -> bool) -> bool {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return false;
    };
    let mut scanned = 0;
    for entry in entries.flatten() {
        scanned += 1;
        if scanned > MAX_SCANNED_MARKERS {
            return true;
        }
        let name = entry.file_name();
        let Some((device, pid)) = name.to_str().and_then(parse_marker_name) else {
            continue;
        };
        if !alive(pid) {
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        if devices.contains(&device) {
            return true;
        }
    }
    false
}

/// The device number of the volume holding `path`.
#[cfg(unix)]
pub(crate) fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|metadata| metadata.dev())
}

#[cfg(not(unix))]
pub(crate) fn device_of(_path: &Path) -> Option<u64> {
    None
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 checks existence and permission without
    // delivering a signal; it takes the pid by value and touches no memory.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

fn marker_directory() -> Option<PathBuf> {
    #[cfg(test)]
    {
        // Tests never write into the real host state directory.
        tests::DIRECTORY_OVERRIDE.with(|directory| directory.borrow().clone())
    }
    #[cfg(not(test))]
    {
        crate::host_state::default_host_state_dir().map(|directory| directory.join(DIRECTORY))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        pub(crate) static DIRECTORY_OVERRIDE: RefCell<Option<PathBuf>> =
            const { RefCell::new(None) };
    }

    #[test]
    fn marker_names_round_trip_and_reject_other_files() {
        assert_eq!(parse_marker_name(&marker_name(42, 7, 3)), Some((42, 7)));
        assert_eq!(parse_marker_name("42-7"), None);
        assert_eq!(parse_marker_name("42-7-3-1"), None);
        assert_eq!(parse_marker_name(".DS_Store"), None);
    }

    /// A live marker on the deleter's volume makes it yield; one on another
    /// volume does not; a dead process's marker counts for nothing and is
    /// removed; dropping the mark ends the yield.
    #[cfg(unix)]
    #[test]
    fn only_a_live_marker_on_the_same_volume_counts() {
        let host = tempfile::tempdir().unwrap();
        let directory = host.path().join(DIRECTORY);
        let worktree = tempfile::tempdir().unwrap();
        let device = device_of(worktree.path()).expect("a unix path has a device");
        let mark = ForegroundIoMark::enter_in(&directory, "preparation", worktree.path())
            .expect("marker written");
        assert!(active_on_in(&directory, &[device], |_| true));
        assert!(!active_on_in(&directory, &[device.wrapping_add(1)], |_| {
            true
        }));

        let dead = directory.join(marker_name(device, 999_999, 0));
        std::fs::write(&dead, "gate").unwrap();
        drop(mark);
        assert!(!active_on_in(&directory, &[device], |pid| pid != 999_999));
        assert!(!dead.exists(), "a dead process's marker is removed");
        assert!(!active_on_in(&directory, &[device], |_| true));
    }

    #[test]
    fn no_marker_directory_means_no_foreground_work() {
        let host = tempfile::tempdir().unwrap();
        assert!(!active_on_in(&host.path().join("absent"), &[1], |_| true));
        assert!(!active_on(&[1]));
    }
}
