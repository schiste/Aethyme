//! Per-component logs (#1222, #1522).
//!
//! Every component's runtime stdout and stderr go to
//! `<host state>/environments/<env_id>/<component>.log`. Each hook phase
//! (`build`, `provision`, `template`, `stop`, `destroy`) writes its own
//! `<component>.<phase>.log` beside it, so the output of a long build never
//! buries the dev server's. A component that fails its health check is
//! otherwise undebuggable by the agent that started it.
//!
//! Aethyme's own `[aethyme]` lines carry an RFC 3339 timestamp, and every hook
//! run starts with a `<label>: started` marker, so a run lines up with the
//! environment's history and `env logs --run last` can select it.
//!
//! Logs are size-capped: past `max_bytes` a log is rotated to `.log.1`, older
//! rotations shift up, and at most `keep` of them are kept (host
//! `environments.toml`, `[logs]`; default 8 MiB and 1). They are removed with
//! the environment.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;

use super::store::valid_env_id;
use super::validate::valid_name;

/// Default per-file cap before rotation.
pub const LOG_CAP_BYTES: u64 = 8 * 1024 * 1024;
/// Default number of rotated files kept per log.
pub const DEFAULT_LOG_KEEP: u32 = 1;
/// Most bytes read from the end of a log to answer a tail.
const TAIL_WINDOW_BYTES: u64 = 256 * 1024;

pub const ENVIRONMENTS_DIRECTORY: &str = "environments";

/// The runtime log's phase name.
pub const RUN_PHASE: &str = "run";
/// Hook phases with a log of their own.
pub const HOOK_PHASES: &[&str] = &["build", "provision", "template", "stop", "destroy"];

/// How large one log grows and how many rotations are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogRetention {
    pub max_bytes: u64,
    /// Rotated files kept; zero discards a full log instead of rotating it.
    pub keep: u32,
}

impl Default for LogRetention {
    fn default() -> Self {
        Self {
            max_bytes: LOG_CAP_BYTES,
            keep: DEFAULT_LOG_KEEP,
        }
    }
}

/// This host's retention, read once per process from the host
/// `environments.toml` (a long-lived supervisor picks up a change on its next
/// start). An unreadable file is the default.
pub fn retention() -> LogRetention {
    static RETENTION: OnceLock<LogRetention> = OnceLock::new();
    *RETENTION.get_or_init(|| {
        environments_directory()
            .map(|root| super::host_file_beside(&root).logs.retention())
            .unwrap_or_default()
    })
}

pub fn environments_directory() -> Option<PathBuf> {
    crate::host_state::default_host_state_dir().map(|d| d.join(ENVIRONMENTS_DIRECTORY))
}

/// The environment's private directory. `None` for anything that is not a
/// well-formed id, so an id can never escape the directory.
pub fn environment_directory(root: &Path, env_id: &str) -> Option<PathBuf> {
    valid_env_id(env_id).then(|| root.join(env_id))
}

pub fn log_path(root: &Path, env_id: &str, component: &str) -> Option<PathBuf> {
    phase_log_path(root, env_id, component, RUN_PHASE)
}

/// `<component>.log` for the runtime, `<component>.<phase>.log` for a hook.
pub fn phase_file_name(component: &str, phase: &str) -> String {
    if phase == RUN_PHASE {
        format!("{component}.log")
    } else {
        format!("{component}.{phase}.log")
    }
}

pub fn phase_log_path(root: &Path, env_id: &str, component: &str, phase: &str) -> Option<PathBuf> {
    if !valid_name(component) || !(phase == RUN_PHASE || HOOK_PHASES.contains(&phase)) {
        return None;
    }
    environment_directory(root, env_id).map(|d| d.join(phase_file_name(component, phase)))
}

fn rotated(path: &Path, generation: u32) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{generation}"));
    PathBuf::from(name)
}

/// Rotate `path` if it is over `retention.max_bytes`: `.1` becomes `.2` and
/// so on up to `keep`, the log becomes `.1`, and anything older is removed.
pub fn rotate_if_needed(path: &Path, retention: LogRetention) -> io::Result<()> {
    if !fs::metadata(path).is_ok_and(|m| m.len() > retention.max_bytes) {
        return Ok(());
    }
    // Generations beyond `keep` (a lowered setting) go first.
    let mut stale = retention.keep + 1;
    while rotated(path, stale).exists() {
        fs::remove_file(rotated(path, stale))?;
        stale += 1;
    }
    if retention.keep == 0 {
        return fs::remove_file(path);
    }
    for generation in (1..retention.keep).rev() {
        let from = rotated(path, generation);
        if from.exists() {
            fs::rename(&from, rotated(path, generation + 1))?;
        }
    }
    fs::rename(path, rotated(path, 1))
}

/// Open a log for appending, rotating it first if it is over the host's
/// retention. Both stdout and stderr of a child are pointed at the result.
pub fn open_for_append(path: &Path) -> io::Result<File> {
    open_for_append_with(path, retention())
}

/// [`open_for_append`] with an explicit retention.
pub fn open_for_append_with(path: &Path, retention: LogRetention) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        crate::host_state::protect_host_state_path(parent, true)?;
    }
    rotate_if_needed(path, retention)?;
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    crate::host_state::protect_host_state_path(path, false)?;
    Ok(file)
}

/// The prefix of an Aethyme marker line written at `now_ms`.
pub fn marker_prefix(now_ms: i64) -> String {
    format!(
        "[aethyme {}]",
        crate::cli::insights::timestamp_millis(now_ms)
    )
}

/// Timestamp every `[aethyme] ` marker line in `text` (#1522). Output lines
/// a hook printed are left as they are.
pub fn stamp_markers(text: &str, now_ms: i64) -> String {
    if !text.contains("[aethyme] ") {
        return text.to_string();
    }
    let prefix = format!("{} ", marker_prefix(now_ms));
    text.split_inclusive('\n')
        .map(|line| match line.strip_prefix("[aethyme] ") {
            Some(rest) => format!("{prefix}{rest}"),
            None => line.to_string(),
        })
        .collect()
}

/// Whether `line` is the marker that starts a run: `[aethyme <time>] <label>:
/// started` before a hook, `... <component>: start` before a process.
fn starts_run(line: &str) -> bool {
    line.starts_with("[aethyme ") && (line.ends_with(": started") || line.ends_with(": start"))
}

/// Where a hook run's output begins in its log, for history entries that
/// point at the output of a failure (#1522).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LogRun {
    pub path: PathBuf,
    /// Byte offset of the run's start marker.
    pub offset: u64,
}

impl LogRun {
    /// The run about to be appended to `path`.
    pub fn next(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            offset: fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        }
    }
}

/// `detail` with a `log` entry naming the run that holds the failure's
/// output, when there is one (#1522).
pub fn with_log(mut detail: serde_json::Value, run: Option<&LogRun>) -> serde_json::Value {
    if let (Some(run), Some(object)) = (run, detail.as_object_mut())
        && let Ok(value) = serde_json::to_value(run)
    {
        object.insert("log".into(), value);
    }
    detail
}

/// The last `lines` lines, reading at most a bounded window from the end of
/// the current log and, when that is short, of the newest rotated one.
pub fn tail(path: &Path, lines: usize) -> io::Result<Vec<String>> {
    let mut collected = read_tail(path, lines)?;
    if collected.len() < lines {
        let mut older = read_tail(&rotated(path, 1), lines - collected.len())?;
        older.append(&mut collected);
        collected = older;
    }
    Ok(collected)
}

/// [`tail`] limited to the last run: from the last start marker on, when
/// one is within the tail window.
pub fn tail_last_run(path: &Path, lines: usize) -> io::Result<Vec<String>> {
    let window = read_tail(path, usize::MAX)?;
    let from = window
        .iter()
        .rposition(|line| starts_run(line))
        .unwrap_or(0);
    let run = &window[from..];
    let skip = run.len().saturating_sub(lines);
    Ok(run[skip..].to_vec())
}

fn read_tail(path: &Path, lines: usize) -> io::Result<Vec<String>> {
    if lines == 0 {
        return Ok(Vec::new());
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let length = file.metadata()?.len();
    let start = length.saturating_sub(TAIL_WINDOW_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(TAIL_WINDOW_BYTES).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut all: Vec<&str> = text.lines().collect();
    if start > 0 && !all.is_empty() {
        // The first line of a mid-file window is probably partial.
        all.remove(0);
    }
    let skip = all.len().saturating_sub(lines);
    Ok(all[skip..].iter().map(|line| line.to_string()).collect())
}

/// Remove an environment's directory (logs and markers). Missing is fine.
pub fn remove_environment_directory(root: &Path, env_id: &str) -> io::Result<()> {
    let Some(directory) = environment_directory(root, env_id) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not an environment id",
        ));
    };
    match fs::remove_dir_all(&directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_timestamped_and_hook_output_is_not() {
        let text =
            "[aethyme] api: build: started\npip install x\n[aethyme] api: build: exit status: 0\n";
        let stamped = stamp_markers(text, 1_794_096_000_123);
        assert_eq!(
            stamped,
            "[aethyme 2026-11-08T00:00:00.123Z] api: build: started\npip install x\n[aethyme 2026-11-08T00:00:00.123Z] api: build: exit status: 0\n"
        );
    }

    #[test]
    fn rotation_keeps_the_configured_number_of_generations() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("api.log");
        let retention = LogRetention {
            max_bytes: 4,
            keep: 2,
        };
        for round in 0..4 {
            fs::write(&log, format!("round {round}")).unwrap();
            rotate_if_needed(&log, retention).unwrap();
        }
        assert!(!log.exists());
        assert_eq!(fs::read_to_string(rotated(&log, 1)).unwrap(), "round 3");
        assert_eq!(fs::read_to_string(rotated(&log, 2)).unwrap(), "round 2");
        assert!(!rotated(&log, 3).exists(), "older generations are dropped");

        fs::write(&log, "small").unwrap();
        rotate_if_needed(
            &log,
            LogRetention {
                max_bytes: 100,
                keep: 2,
            },
        )
        .unwrap();
        assert!(log.exists(), "under the cap nothing rotates");

        rotate_if_needed(
            &log,
            LogRetention {
                max_bytes: 1,
                keep: 0,
            },
        )
        .unwrap();
        assert!(!log.exists(), "keep = 0 discards a full log");
        assert!(
            !rotated(&log, 1).exists() && !rotated(&log, 2).exists(),
            "a lowered keep drops generations beyond it"
        );
    }

    #[test]
    fn the_last_run_starts_at_its_marker() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("api.build.log");
        fs::write(
            &log,
            "[aethyme t1] api: build: started\nold output\n[aethyme t2] api: build: started\nnew output\n[aethyme t3] api: build: exit status: 1\n",
        )
        .unwrap();
        assert_eq!(
            tail_last_run(&log, 50).unwrap(),
            vec![
                "[aethyme t2] api: build: started",
                "new output",
                "[aethyme t3] api: build: exit status: 1"
            ]
        );
        assert_eq!(tail(&log, 2).unwrap().len(), 2);
    }

    #[test]
    fn only_known_phases_have_a_log_path() {
        let root = Path::new("/h/environments");
        let id = "env-0123456789ab";
        if valid_env_id(id) {
            assert_eq!(
                phase_log_path(root, id, "api", "build").unwrap(),
                root.join(id).join("api.build.log")
            );
            assert_eq!(
                log_path(root, id, "api").unwrap(),
                root.join(id).join("api.log")
            );
            assert!(phase_log_path(root, id, "api", "../x").is_none());
        }
    }
}
