//! Shared location and permission policy for per-user, host-scoped state.
//!
//! Repository-local broker state belongs under `.aethyme/`. Coordination that
//! must span independent clones belongs here instead. Callers choose their own
//! database filename beneath this directory.

use std::path::{Path, PathBuf};

pub(crate) fn default_host_state_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AETHYME_HOST_STATE_DIR").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("XDG_STATE_HOME").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path).join("aethyme"));
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Aethyme")
    } else {
        home.join(".local/state/aethyme")
    })
}

#[cfg(unix)]
pub(crate) fn protect_host_state_path(path: &Path, directory: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = if directory { 0o700 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
pub(crate) fn protect_host_state_path(_path: &Path, _directory: bool) -> std::io::Result<()> {
    Ok(())
}
