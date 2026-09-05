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

/// Explain a host-state I/O failure, naming the usual cause when access is denied.
///
/// Coordination state is host-scoped by design: it must be visible to every
/// worktree and clone, so it lives outside all of them. A sandbox that confines
/// writes to the invoking checkout therefore denies it, and the bare
/// `Operation not permitted` this surfaces reads as a broken installation. It
/// is not: the binary is fine and the path is usually owned correctly.
pub(crate) fn describe_host_state_io(path: &Path, source: &std::io::Error) -> String {
    let base = format!("{}: {source}", path.display());
    if !access_was_denied(source) {
        return base;
    }
    format!(
        "{base} — Aethyme coordination state is host-scoped and lives outside any single \
         worktree, so a sandbox or permission policy that confines writes to the checkout \
         denies it. This is not a missing or outdated installation. Point \
         AETHYME_HOST_STATE_DIR at a writable location, or grant this process access to \
         that path."
    )
}

/// EPERM and EACCES both mean "denied"; match the kind and the raw errno so a
/// platform that maps either differently still reports the useful message.
fn access_was_denied(source: &std::io::Error) -> bool {
    matches!(source.kind(), std::io::ErrorKind::PermissionDenied)
        || matches!(source.raw_os_error(), Some(1) | Some(13))
}

/// The same explanation for a SQLite open failure, which is how the denial
/// surfaces when it happens inside the database layer rather than a bare open.
pub(crate) fn describe_host_state_sqlite(error: &rusqlite::Error) -> String {
    let base = error.to_string();
    let cannot_open = matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _)
            if inner.code == rusqlite::ErrorCode::CannotOpen
                || inner.code == rusqlite::ErrorCode::PermissionDenied
    );
    if !cannot_open {
        return base;
    }
    format!(
        "{base} — the host-scoped coordination database could not be opened. A sandbox or \
         permission policy that confines writes to the invoking checkout denies it; this is \
         not a missing or outdated installation. Point AETHYME_HOST_STATE_DIR at a writable \
         location, or grant this process access."
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A denial must say it is a denial, and must not read as a broken install
    /// (#132: the caller's wrapper turned this into "install or upgrade Aethyme").
    #[test]
    fn denied_host_state_access_names_the_cause_and_the_override() {
        for source in [
            std::io::Error::from_raw_os_error(1),  // EPERM, what macOS reports
            std::io::Error::from_raw_os_error(13), // EACCES
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
        ] {
            let text = describe_host_state_io(Path::new("/host/state"), &source);
            assert!(text.contains("host-scoped"), "{text}");
            assert!(text.contains("AETHYME_HOST_STATE_DIR"), "{text}");
            assert!(
                text.contains("not a missing or outdated installation"),
                "the message must rule out the wrong remediation: {text}"
            );
        }
    }

    /// Every other I/O failure stays exactly as terse as before.
    #[test]
    fn other_host_state_errors_are_not_embellished() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let text = describe_host_state_io(Path::new("/host/state"), &source);
        assert_eq!(text, "/host/state: missing");
    }

    #[test]
    fn a_sqlite_open_failure_explains_itself_and_others_do_not() {
        let denied = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::CannotOpen,
                extended_code: 14,
            },
            Some("unable to open database file".into()),
        );
        let text = describe_host_state_sqlite(&denied);
        assert!(text.contains("AETHYME_HOST_STATE_DIR"), "{text}");
        assert!(text.contains("not a missing or outdated installation"), "{text}");

        let unrelated = rusqlite::Error::QueryReturnedNoRows;
        assert_eq!(
            describe_host_state_sqlite(&unrelated),
            unrelated.to_string(),
            "unrelated database errors must not gain sandbox advice"
        );
    }
}
