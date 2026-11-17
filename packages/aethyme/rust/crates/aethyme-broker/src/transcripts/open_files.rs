//! Whether any process has a transcript open.
//!
//! One pass shares one host-wide open-file observation across every
//! transcript it judges, and takes a new one only once the last is older
//! than the refresh interval ([`OpenFiles`]): listing every open file on a
//! host costs most of a second, and taking one per transcript was most of
//! an archiving pass.
//!
//! Linux reads `/proc/<pid>/fd` directly. Other Unix hosts use the broker's
//! bounded, NUL-delimited, host-wide `lsof` observation, the one checkout
//! retirement shares across a run
//! (`session_holder::retirement_run_process_use_snapshot`), whose parser
//! anchors every field, so a file name cannot forge a record. An incomplete
//! observation is never read as "nothing is open": every transcript judged
//! against it is skipped as `open_check_unavailable`.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::eligibility::OpenState;

/// How long one host-wide `lsof` observation may run.
#[cfg_attr(target_os = "linux", allow(dead_code))]
const OBSERVATION_BUDGET: Duration = Duration::from_secs(10);

/// One open descriptor: which process, which file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenFile {
    pub pid: u32,
    pub path: PathBuf,
}

/// A source of open-file observations.
pub(crate) trait OpenFileProbe {
    /// Every file open on the host, or why the observation is incomplete.
    fn observe(&mut self) -> Result<Vec<OpenFile>, String>;
}

/// The host's own process table.
#[derive(Debug, Default)]
pub(crate) struct SystemProbe;

impl OpenFileProbe for SystemProbe {
    fn observe(&mut self) -> Result<Vec<OpenFile>, String> {
        #[cfg(target_os = "linux")]
        {
            proc_open_under(Path::new("/proc"), Path::new("/"))
        }
        #[cfg(not(target_os = "linux"))]
        {
            match crate::session_holder::retirement_run_process_use_snapshot(OBSERVATION_BUDGET) {
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

/// The clock observations are aged by.
fn now() -> Instant {
    Instant::now()
}

/// One host-wide observation and what was judged against it.
struct Observation {
    taken: Instant,
    /// Every open file, or why the observation is incomplete.
    files: Result<Vec<OpenFile>, String>,
    /// The open paths, for a constant-time lookup per transcript.
    paths: HashSet<PathBuf>,
    judged: u64,
}

/// Open-file observations shared across a pass.
///
/// A transcript is never judged against an observation older than
/// `refresh_after`; an older one is replaced first. An incomplete
/// observation answers [`OpenState::Unknown`] for every transcript, so
/// nothing judged against it is archived or removed (fail closed).
///
/// The shared observation only decides which transcripts are worth
/// compressing. Before originals are removed the archiver [`renew`]s it, so
/// a batch of removals is judged against one observation taken after the
/// batch's last copy was verified. A process that opens a transcript after
/// that observation changes its size or modification time only once it
/// writes, which the archiver re-checks immediately before each unlink; one
/// that opens it and has not yet written by the unlink is the residual
/// window, now one listing per batch rather than one per transcript.
///
/// [`renew`]: OpenFiles::renew
pub(crate) struct OpenFiles<P> {
    probe: P,
    refresh_after: Duration,
    observation: Option<Observation>,
    /// Observations taken, and the time they took (telemetry).
    pub probes: u64,
    pub probe_time: Duration,
    /// Observations taken because the previous one had grown too old.
    pub refreshes: u64,
    /// Observations asked for by [`renew`](Self::renew) (one per removal
    /// batch).
    pub renewals: u64,
    /// Transcripts judged against an observation, in total and at most per
    /// observation.
    pub judged: u64,
    pub max_judged_per_observation: u64,
}

impl<P: OpenFileProbe> OpenFiles<P> {
    pub fn new(probe: P, refresh_after: Duration) -> Self {
        Self {
            probe,
            refresh_after,
            observation: None,
            probes: 0,
            probe_time: Duration::ZERO,
            refreshes: 0,
            renewals: 0,
            judged: 0,
            max_judged_per_observation: 0,
        }
    }

    fn take(&mut self) -> Observation {
        let started = now();
        let files = self.probe.observe();
        self.probes += 1;
        self.probe_time += now().saturating_duration_since(started);
        let paths = match &files {
            Ok(files) => files.iter().map(|file| file.path.clone()).collect(),
            Err(_) => HashSet::new(),
        };
        // Aged from when it started: it may reflect any moment of its run.
        Observation {
            taken: started,
            files,
            paths,
            judged: 0,
        }
    }

    /// The current observation, replaced first when older than
    /// `refresh_after`.
    fn observation(&mut self) -> &mut Observation {
        let stale = self.observation.as_ref().is_some_and(|observation| {
            now().saturating_duration_since(observation.taken) > self.refresh_after
        });
        if stale {
            self.refreshes += 1;
        }
        match self.observation.take() {
            Some(current) if !stale => self.observation.insert(current),
            _ => {
                let fresh = self.take();
                self.observation.insert(fresh)
            }
        }
    }

    /// Whether `path` is open, from an observation at most `refresh_after`
    /// old (the one [`renew`](Self::renew) asked for, when it did).
    pub fn state_of(&mut self, path: &Path) -> OpenState {
        let canonical = fs::canonicalize(path).ok();
        let observation = self.observation();
        observation.judged += 1;
        let judged = observation.judged;
        let state = match &observation.files {
            Err(_) => OpenState::Unknown,
            Ok(_) => {
                let open = observation.paths.contains(path)
                    || canonical
                        .as_ref()
                        .is_some_and(|canonical| observation.paths.contains(canonical));
                if open {
                    OpenState::Open
                } else {
                    OpenState::Closed
                }
            }
        };
        self.judged += 1;
        self.max_judged_per_observation = self.max_judged_per_observation.max(judged);
        state
    }

    /// Make the next judgement take a new observation: used once per batch
    /// of removals, after the batch's last copy was verified.
    pub fn renew(&mut self) {
        self.observation = None;
        self.renewals += 1;
    }

    /// Whether `path` is open, from an observation taken now. Used by
    /// restore, which judges one file and can afford it.
    pub fn fresh_state_of(&mut self, path: &Path) -> OpenState {
        self.renew();
        self.state_of(path)
    }

    /// Every open file beneath `root`, from an observation at most
    /// `refresh_after` old.
    pub fn holders(&mut self, root: &Path) -> Result<Vec<OpenFile>, String> {
        let canonical_root = fs::canonicalize(root).ok();
        let files = self
            .observation()
            .files
            .as_ref()
            .map_err(|error| error.clone())?;
        Ok(files
            .iter()
            .filter(|file| descends(&file.path, root, canonical_root.as_deref()))
            .cloned()
            .collect())
    }
}
