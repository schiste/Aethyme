//! Whether any process has a transcript open.
//!
//! Linux reads `/proc/<pid>/fd` directly. Other Unix hosts use the broker's
//! bounded, NUL-delimited `lsof` snapshot (`reclaim::open_file_snapshot_under`),
//! whose parser anchors every field, so a file name cannot forge a record. An
//! incomplete listing is never read as "nothing is open": the transcript is
//! skipped as `open_check_unavailable`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::eligibility::OpenState;

/// One open descriptor: which process, which file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenFile {
    pub pid: u32,
    pub path: PathBuf,
}

/// A source of open-file listings.
pub(crate) trait OpenFileProbe {
    /// Every file open beneath `root`, or why the listing is incomplete.
    fn open_under(&mut self, root: &Path) -> Result<Vec<OpenFile>, String>;
}

/// The host's own process table.
#[derive(Debug, Default)]
pub(crate) struct SystemProbe;

impl OpenFileProbe for SystemProbe {
    fn open_under(&mut self, root: &Path) -> Result<Vec<OpenFile>, String> {
        #[cfg(target_os = "linux")]
        {
            proc_open_under(Path::new("/proc"), root)
        }
        #[cfg(not(target_os = "linux"))]
        {
            match crate::reclaim::open_file_snapshot_under(root) {
                crate::reclaim::OpenFileSnapshot::Complete(uses) => Ok(uses
                    .into_iter()
                    .map(|open| OpenFile {
                        pid: open.pid,
                        path: open.path,
                    })
                    .collect()),
                crate::reclaim::OpenFileSnapshot::Incomplete { reason, .. } => Err(reason),
            }
        }
    }
}

fn descends(path: &Path, root: &Path, canonical_root: Option<&Path>) -> bool {
    path.starts_with(root) || canonical_root.is_some_and(|canonical| path.starts_with(canonical))
}

/// Files process `pid` has open beneath `root`, read from
/// `<proc_root>/<pid>/fd`. A process that exited or belongs to another user
/// contributes nothing.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn pid_open_under(proc_root: &Path, pid: u32, root: &Path) -> Vec<PathBuf> {
    let canonical_root = fs::canonicalize(root).ok();
    let Ok(descriptors) = fs::read_dir(proc_root.join(pid.to_string()).join("fd")) else {
        return Vec::new();
    };
    descriptors
        .filter_map(Result::ok)
        .filter_map(|descriptor| fs::read_link(descriptor.path()).ok())
        .filter(|target| descends(target, root, canonical_root.as_deref()))
        .collect()
}

/// Every file open beneath `root` according to `<proc_root>/*/fd`.
///
/// A process that exits mid-listing, or whose descriptors belong to another
/// user (`EACCES`), is skipped: an unprivileged agent cannot see them, and
/// the transcripts this lane archives are the user's own. Any other failure
/// to list makes the whole listing incomplete.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn proc_open_under(proc_root: &Path, root: &Path) -> Result<Vec<OpenFile>, String> {
    let canonical_root = fs::canonicalize(root).ok();
    let processes = fs::read_dir(proc_root)
        .map_err(|error| format!("cannot list {}: {error}", proc_root.display()))?;
    let mut found = Vec::new();
    for process in processes {
        let process =
            process.map_err(|error| format!("cannot list {}: {error}", proc_root.display()))?;
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let fd_directory = process.path().join("fd");
        let descriptors = match fs::read_dir(&fd_directory) {
            Ok(descriptors) => descriptors,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(error) => {
                return Err(format!("cannot list {}: {error}", fd_directory.display()));
            }
        };
        for descriptor in descriptors.filter_map(Result::ok) {
            let Ok(target) = fs::read_link(descriptor.path()) else {
                continue;
            };
            if descends(&target, root, canonical_root.as_deref()) {
                found.push(OpenFile { pid, path: target });
            }
        }
    }
    Ok(found)
}

struct Listing {
    root: PathBuf,
    taken: Instant,
    files: Result<Vec<OpenFile>, String>,
}

/// Open-file listings per root, reused while younger than `max_age`.
pub(crate) struct OpenFiles<P> {
    probe: P,
    max_age: Duration,
    listings: Vec<Listing>,
    /// Listings taken, and the time they took (telemetry).
    pub probes: u64,
    pub probe_time: Duration,
}

impl<P: OpenFileProbe> OpenFiles<P> {
    pub fn new(probe: P, max_age: Duration) -> Self {
        Self {
            probe,
            max_age,
            listings: Vec::new(),
            probes: 0,
            probe_time: Duration::ZERO,
        }
    }

    fn take(&mut self, root: &Path) -> Listing {
        let started = Instant::now();
        let files = self.probe.open_under(root);
        self.probes += 1;
        self.probe_time += started.elapsed();
        Listing {
            root: root.to_path_buf(),
            taken: Instant::now(),
            files,
        }
    }

    fn listing(&mut self, root: &Path) -> &Result<Vec<OpenFile>, String> {
        let position = self
            .listings
            .iter()
            .position(|listing| listing.root == root);
        let index = match position {
            Some(index) if self.listings[index].taken.elapsed() <= self.max_age => index,
            Some(index) => {
                let listing = self.take(root);
                self.listings[index] = listing;
                index
            }
            None => {
                let listing = self.take(root);
                self.listings.push(listing);
                self.listings.len() - 1
            }
        };
        &self.listings[index].files
    }

    /// Whether `path` (beneath `root`) is open, from a listing at most
    /// `max_age` old.
    pub fn state_of(&mut self, root: &Path, path: &Path) -> OpenState {
        let canonical = fs::canonicalize(path).ok();
        match self.listing(root) {
            Err(_) => OpenState::Unknown,
            Ok(files) => {
                let open = files
                    .iter()
                    .any(|file| file.path == path || canonical.as_ref() == Some(&file.path));
                if open {
                    OpenState::Open
                } else {
                    OpenState::Closed
                }
            }
        }
    }

    /// Every open file beneath `root`, from a listing at most `max_age` old.
    pub fn holders(&mut self, root: &Path) -> Result<Vec<OpenFile>, String> {
        self.listing(root).clone()
    }
}
