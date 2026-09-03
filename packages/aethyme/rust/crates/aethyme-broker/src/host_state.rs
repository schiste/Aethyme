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

/// True when the host state directory was named explicitly rather than derived
/// from the platform default.
///
/// An explicit declaration is a deliberate choice about where durable state
/// lives, so it outranks the ephemeral-repository guard below.
pub(crate) fn host_state_dir_is_explicit() -> bool {
    ["AETHYME_HOST_STATE_DIR", "XDG_STATE_HOME"]
        .into_iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// True when `path` resolves inside the system temporary directory.
///
/// Repositories under the system temp directory are ephemeral - test fixtures
/// and scratch clones - and must never anchor worktrees in durable host state.
/// Worktree storage is host-scoped while the records that own it are
/// repository-local, so when such a repository is deleted its worktree tree is
/// left with no database that could ever account for it again.
pub(crate) fn path_is_ephemeral(path: &Path) -> bool {
    let temp = std::env::temp_dir();
    let temp = std::fs::canonicalize(&temp).unwrap_or(temp);
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    path.starts_with(&temp)
}

pub(crate) fn default_host_cache_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AETHYME_HOST_CACHE_DIR").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("XDG_CACHE_HOME").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path).join("aethyme"));
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Caches/Aethyme")
    } else {
        home.join(".cache/aethyme")
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
