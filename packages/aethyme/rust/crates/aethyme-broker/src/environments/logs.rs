//! Per-component logs (#1222).
//!
//! Every component's stdout and stderr go to
//! `<host state>/environments/<env_id>/<component>.log`. A component that fails
//! its health check is otherwise undebuggable by the agent that started it.
//! Logs are size-capped: when a log passes the cap it is rotated to `.log.1`
//! (replacing the previous one), so one environment holds at most twice the
//! cap per component. They are removed with the environment.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::store::valid_env_id;
use super::validate::valid_name;

/// Per-file cap before rotation.
pub const LOG_CAP_BYTES: u64 = 8 * 1024 * 1024;
/// Most bytes read from the end of a log to answer a tail.
const TAIL_WINDOW_BYTES: u64 = 256 * 1024;

pub const ENVIRONMENTS_DIRECTORY: &str = "environments";

pub fn environments_directory() -> Option<PathBuf> {
    crate::host_state::default_host_state_dir().map(|d| d.join(ENVIRONMENTS_DIRECTORY))
}

/// The environment's private directory. `None` for anything that is not a
/// well-formed id, so an id can never escape the directory.
pub fn environment_directory(root: &Path, env_id: &str) -> Option<PathBuf> {
    valid_env_id(env_id).then(|| root.join(env_id))
}

pub fn log_path(root: &Path, env_id: &str, component: &str) -> Option<PathBuf> {
    if !valid_name(component) {
        return None;
    }
    environment_directory(root, env_id).map(|d| d.join(format!("{component}.log")))
}

fn rotated(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// Open a component log for appending, rotating it first if it is over
/// `cap`. Both stdout and stderr of a child are pointed at the result.
pub fn open_for_append(path: &Path, cap: u64) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        crate::host_state::protect_host_state_path(parent, true)?;
    }
    if fs::metadata(path).map(|m| m.len() > cap).unwrap_or(false) {
        fs::rename(path, rotated(path))?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    crate::host_state::protect_host_state_path(path, false)?;
    Ok(file)
}

/// The last `lines` lines, reading at most a bounded window from the end of
/// the current log and, when that is short, of the rotated one.
pub fn tail(path: &Path, lines: usize) -> io::Result<Vec<String>> {
    let mut collected = read_tail(path, lines)?;
    if collected.len() < lines {
        let mut older = read_tail(&rotated(path), lines - collected.len())?;
        older.append(&mut collected);
        collected = older;
    }
    Ok(collected)
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
