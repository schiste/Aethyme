//! Absolute paths for the system utilities the broker spawns on hot paths.
//!
//! `Command::new("ps")` resolves through PATH on every spawn. On a developer
//! machine PATH holds a dozen or more entries ahead of `/bin` (shims, version
//! managers, package managers), and a process-exec trace showed every `ps`
//! call failing through about fourteen of them before reaching `/bin/ps`. The
//! utility's location does not change during a process, so resolve it once.
//!
//! The fixed locations are the operating system's own; PATH stays the
//! fallback for a system that keeps them elsewhere.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Test seam: a `ps` stub for integration tests that observe how the broker
/// calls it. Honoured only in a process started by the test harness, so an
/// ordinary environment cannot redirect it.
pub(crate) const TEST_PS_ENV: &str = "AETHYME_TEST_PS";

/// `ps`: `/bin/ps` on macOS and most Linux systems, `/usr/bin/ps` on others.
pub(crate) fn ps() -> &'static Path {
    static PS: OnceLock<PathBuf> = OnceLock::new();
    PS.get_or_init(|| {
        test_override(TEST_PS_ENV).unwrap_or_else(|| resolve("ps", &["/bin/ps", "/usr/bin/ps"]))
    })
}

fn test_override(name: &str) -> Option<PathBuf> {
    let under_test = ["CARGO_MANIFEST_DIR", "CARGO_BIN_EXE_aethyme"]
        .iter()
        .any(|key| std::env::var_os(key).is_some());
    under_test
        .then(|| std::env::var_os(name))
        .flatten()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// `sysctl`: `/usr/sbin/sysctl` on macOS, `/sbin/sysctl` on Linux.
pub(crate) fn sysctl() -> &'static Path {
    static SYSCTL: OnceLock<PathBuf> = OnceLock::new();
    SYSCTL.get_or_init(|| resolve("sysctl", &["/usr/sbin/sysctl", "/sbin/sysctl"]))
}

/// The first of `fixed` that is an executable file, else the bare `name` so
/// the spawn falls back to PATH exactly as before.
fn resolve(name: &str, fixed: &[&str]) -> PathBuf {
    fixed
        .iter()
        .map(PathBuf::from)
        .find(|path| is_executable_file(path))
        .unwrap_or_else(|| PathBuf::from(name))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn ps_resolves_to_an_absolute_system_path_when_one_exists() {
        let resolved = ps();
        if ["/bin/ps", "/usr/bin/ps"]
            .iter()
            .any(|path| is_executable_file(Path::new(path)))
        {
            assert!(resolved.is_absolute(), "{}", resolved.display());
        } else {
            assert_eq!(resolved, Path::new("ps"));
        }
    }

    #[test]
    fn a_missing_fixed_location_falls_back_to_path_lookup() {
        assert_eq!(
            resolve("tool", &["/nonexistent/aethyme/tool"]),
            PathBuf::from("tool")
        );
    }
}
