//! Which volumes foreground work is writing to right now.
//!
//! A dependency install or a gate is work an agent waits on. Unattended bulk
//! deletion on the same volume competes with it for the disk: measured on an
//! external volume, a 30-minute offline install used about 6 s of child CPU
//! while the checkout trash catch-up removed 17.7 GB beside it. The install
//! was starved for I/O, not CPU.
//!
//! Foreground work leaves one marker file per run in the host state
//! directory, named for the volume (device number) it writes to, its process
//! and that process's start time. Unattended deleters read the directory
//! once per slice and yield the disk while a live marker names a volume they
//! delete on. A marker counts for nothing, and is removed by the next reader,
//! when its process is gone, when the pid now names another process (its
//! start time differs: a SIGKILLed broker's pid reused), or when it is older
//! than [`MAX_MARKER_AGE`], so a crashed run never pins a deleter in its
//! yielding state.
//!
//! Everything here is advisory and best effort: a marker that cannot be
//! written only means the deleter does not yield.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// The marker directory beneath the host state directory.
const DIRECTORY: &str = "run/foreground-io";

/// At most this many entries are read per check (every one is judged and a
/// dead one pruned); a directory that holds more is treated as "foreground
/// work is running" rather than walked further.
const MAX_SCANNED_MARKERS: usize = 4096;

/// A marker older than this is ignored and removed whatever its process:
/// longer than any preparation step or gate command is expected to run, so
/// only a leaked marker reaches it, and a leak costs a deleter at most this
/// long in its yielding state.
pub(crate) const MAX_MARKER_AGE: Duration = Duration::from_secs(4 * 60 * 60);

/// The process a marker names: its pid and start time (0 when the platform
/// cannot say, which then trusts the pid alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MarkerProcess {
    pub(crate) pid: u32,
    pub(crate) start: u64,
}

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
        let pid = std::process::id();
        let start = process_start(pid).unwrap_or(0);
        let marker = directory.join(marker_name(device, pid, start, sequence));
        std::fs::write(&marker, kind).ok()?;
        crate::phase_telemetry::add("foreground_io_marked", std::time::Duration::ZERO);
        Some(Self { path: marker })
    }
}

impl Drop for ForegroundIoMark {
    fn drop(&mut self) {
        // Best effort: a marker left behind names a process that is gone,
        // which readers ignore and remove.
        std::fs::remove_file(&self.path).ok();
    }
}

fn marker_name(device: u64, pid: u32, start: u64, sequence: u64) -> String {
    format!("{device}-{pid}-{start}-{sequence}")
}

/// `(device, process)` from a marker's file name.
fn parse_marker_name(name: &str) -> Option<(u64, MarkerProcess)> {
    let mut parts = name.split('-');
    let device = parts.next()?.parse().ok()?;
    let pid = parts.next()?.parse().ok()?;
    let start = parts.next()?.parse().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    parts
        .next()
        .is_none()
        .then_some((device, MarkerProcess { pid, start }))
}

/// Whether foreground work runs on any of `devices`. With no host state
/// directory nothing can be known and the answer is `false`.
pub(crate) fn active_on(devices: &[u64]) -> bool {
    marker_directory().is_some_and(|directory| {
        active_on_in(&directory, devices, SystemTime::now(), process_matches)
    })
}

/// Whether `process` is still the process that wrote its marker: alive, and
/// started when the marker says (a reused pid names another process).
pub(crate) fn process_matches(process: MarkerProcess) -> bool {
    process_alive(process.pid)
        && (process.start == 0 || process_start(process.pid) == Some(process.start))
}

/// [`active_on`] for `directory` at `now`, with the process check injected.
/// Every entry read is judged, and a dead, reused or expired marker removed,
/// before the answer is given.
pub(crate) fn active_on_in(
    directory: &Path,
    devices: &[u64],
    now: SystemTime,
    matches: impl Fn(MarkerProcess) -> bool,
) -> bool {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return false;
    };
    let mut active = false;
    for (scanned, entry) in entries.flatten().enumerate() {
        if scanned >= MAX_SCANNED_MARKERS {
            return true;
        }
        let name = entry.file_name();
        let Some((device, process)) = name.to_str().and_then(parse_marker_name) else {
            continue;
        };
        let expired = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > MAX_MARKER_AGE);
        if expired || !matches(process) {
            std::fs::remove_file(entry.path()).ok();
            continue;
        }
        active |= devices.contains(&device);
    }
    active
}

fn process_start(pid: u32) -> Option<u64> {
    crate::gates::process_start_time(i32::try_from(pid).ok()?)
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
        assert_eq!(
            parse_marker_name(&marker_name(42, 7, 99, 3)),
            Some((42, MarkerProcess { pid: 7, start: 99 }))
        );
        assert_eq!(parse_marker_name("42-7-3"), None);
        assert_eq!(parse_marker_name("42-7-99-3-1"), None);
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
        let now = SystemTime::now();
        let mark = ForegroundIoMark::enter_in(&directory, "preparation", worktree.path())
            .expect("marker written");
        assert!(active_on_in(&directory, &[device], now, process_matches));
        assert!(!active_on_in(
            &directory,
            &[device.wrapping_add(1)],
            now,
            process_matches
        ));

        let dead = directory.join(marker_name(device, 999_999, 1, 0));
        std::fs::write(&dead, "gate").unwrap();
        drop(mark);
        assert!(!active_on_in(&directory, &[device], now, |process| {
            process.pid != 999_999
        }));
        assert!(!dead.exists(), "a dead process's marker is removed");
        assert!(!active_on_in(&directory, &[device], now, |_| true));
    }

    /// A SIGKILLed run's pid reused by another process: same pid, another
    /// start time, so the marker is not live and is removed.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_reused_pid_does_not_keep_a_marker_live() {
        let pid = std::process::id();
        let start = process_start(pid).expect("this process has a start time");
        assert!(process_matches(MarkerProcess { pid, start }));
        assert!(!process_matches(MarkerProcess {
            pid,
            start: start.wrapping_add(1),
        }));

        let host = tempfile::tempdir().unwrap();
        let directory = host.path().join(DIRECTORY);
        std::fs::create_dir_all(&directory).unwrap();
        let reused = directory.join(marker_name(5, pid, start.wrapping_add(1), 0));
        std::fs::write(&reused, "preparation").unwrap();
        assert!(!active_on_in(
            &directory,
            &[5],
            SystemTime::now(),
            process_matches
        ));
        assert!(!reused.exists());
    }

    /// A marker older than the cap is ignored and removed even when its
    /// process still matches.
    #[test]
    fn an_expired_marker_is_ignored_and_removed() {
        let host = tempfile::tempdir().unwrap();
        let directory = host.path().join(DIRECTORY);
        std::fs::create_dir_all(&directory).unwrap();
        let marker = directory.join(marker_name(5, 7, 9, 0));
        std::fs::write(&marker, "gate").unwrap();
        let now = SystemTime::now();
        assert!(active_on_in(&directory, &[5], now, |_| true));
        let later = now + MAX_MARKER_AGE + Duration::from_secs(60);
        assert!(!active_on_in(&directory, &[5], later, |_| true));
        assert!(!marker.exists());
    }

    /// Dead markers anywhere in the directory are pruned before the answer,
    /// even when a live one on the volume comes first.
    #[test]
    fn every_dead_marker_is_pruned_before_answering() {
        let host = tempfile::tempdir().unwrap();
        let directory = host.path().join(DIRECTORY);
        std::fs::create_dir_all(&directory).unwrap();
        for sequence in 0..300 {
            std::fs::write(directory.join(marker_name(5, 1, 1, sequence)), "gate").unwrap();
        }
        std::fs::write(directory.join(marker_name(5, 2, 2, 0)), "gate").unwrap();
        assert!(active_on_in(
            &directory,
            &[5],
            SystemTime::now(),
            |process| { process.pid == 2 }
        ));
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    }

    #[test]
    fn no_marker_directory_means_no_foreground_work() {
        let host = tempfile::tempdir().unwrap();
        assert!(!active_on_in(
            &host.path().join("absent"),
            &[1],
            SystemTime::now(),
            |_| true
        ));
        assert!(!active_on(&[1]));
    }
}
