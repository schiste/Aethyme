//! Absolute paths for the system utilities the broker spawns on hot paths.
//!
//! `Command::new("ps")` resolves through PATH on every spawn. On a developer
//! machine PATH holds a dozen or more entries ahead of `/bin` (shims, version
//! managers, package managers), and a process-exec trace showed every `ps`
//! call failing through about fourteen of them before reaching `/bin/ps`. The
//! utility's location does not change during a process, so resolve it once.
//!
//! The fixed locations are the operating system's own and are used only on
//! macOS; PATH stays the fallback, and the rule on Linux (see [`ps`]).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Test seam: a `ps` stub for integration tests that observe how the broker
/// calls it. Compiled only into debug builds (the test profile), and honoured
/// only in a process started by the test harness, so a release binary has no
/// way to redirect it.
#[cfg(debug_assertions)]
pub(crate) const TEST_PS_ENV: &str = "AETHYME_TEST_PS";

/// `ps`. On macOS, `/bin/ps`: the system's only `ps`, and the platform where a
/// long PATH made every spawn walk a dozen entries first. On Linux, PATH order
/// as before: a distribution may ship both busybox `/bin/ps` and procps
/// `/usr/bin/ps`, and the `-o lstart=`/`-ww` columns the broker parses are
/// procps's, so guessing an absolute path could pick the wrong one.
pub(crate) fn ps() -> &'static Path {
    static PS: OnceLock<PathBuf> = OnceLock::new();
    PS.get_or_init(|| {
        #[cfg(debug_assertions)]
        if let Some(stub) = test_override(TEST_PS_ENV) {
            return stub;
        }
        if cfg!(target_os = "macos") {
            resolve("ps", &["/bin/ps"])
        } else {
            PathBuf::from("ps")
        }
    })
}

#[cfg(debug_assertions)]
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

/// `sysctl`, read only on macOS: `/usr/sbin/sysctl`.
pub(crate) fn sysctl() -> &'static Path {
    static SYSCTL: OnceLock<PathBuf> = OnceLock::new();
    SYSCTL.get_or_init(|| resolve("sysctl", &["/usr/sbin/sysctl"]))
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

    #[test]
    fn ps_is_absolute_on_macos_and_path_ordered_elsewhere() {
        let resolved = ps();
        if std::env::var_os("AETHYME_TEST_PS").is_some() {
            return;
        }
        if cfg!(target_os = "macos") && is_executable_file(Path::new("/bin/ps")) {
            assert_eq!(resolved, Path::new("/bin/ps"));
        } else if !cfg!(target_os = "macos") {
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
