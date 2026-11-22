//! Shared identity and launchctl mechanics for reviewed per-user services.
//!
//! Plans, manifests, units, and health records remain purpose-specific so an
//! existing service's serialized contract is not changed by another service.

use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub(crate) const MAX_BINARY_BYTES: u64 = 256 * 1024 * 1024;

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn text(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| invalid("service paths must be UTF-8"))
}

pub(crate) fn xml(value: &str) -> io::Result<String> {
    if value
        .chars()
        .any(|c| c < ' ' && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(invalid("service values contain XML control characters"));
    }
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

pub(crate) fn absolute_directory(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() || !path.is_dir() {
        return Err(invalid(
            "service directory inputs must be existing absolute directories",
        ));
    }
    path.canonicalize()
}

pub(crate) fn executable(path: &Path) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    if !path.is_absolute() {
        return Err(invalid("service executables must use absolute paths"));
    }
    let path = path.canonicalize()?;
    let metadata = path.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(invalid(
            "service executable is not an executable regular file",
        ));
    }
    Ok(path)
}

pub(crate) fn binary_digest(path: &Path) -> io::Result<String> {
    binary_digest_with_hook_inner(path, || Ok(()))
}

fn same_identity(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mode() == b.mode()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}

fn binary_digest_with_hook_inner(
    path: &Path,
    after_open: impl FnOnce() -> io::Result<()>,
) -> io::Result<String> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || before.mode() & 0o111 == 0 {
        return Err(invalid(
            "service identity requires an executable regular file",
        ));
    }
    if before.len() > MAX_BINARY_BYTES {
        return Err(invalid("service executable exceeds the identity budget"));
    }
    after_open()?;
    let mut hash = Sha256::new();
    let mut read = (&file).take(MAX_BINARY_BYTES + 1);
    let mut bytes = [0; 64 * 1024];
    let mut total = 0;
    loop {
        let size = read.read(&mut bytes)?;
        if size == 0 {
            break;
        }
        total += size as u64;
        if total > MAX_BINARY_BYTES {
            return Err(invalid(
                "service executable grew beyond the identity budget",
            ));
        }
        hash.update(&bytes[..size]);
    }
    let after = file.metadata()?;
    let named = std::fs::symlink_metadata(path)?;
    if total != before.len() || !same_identity(&before, &after) || !same_identity(&after, &named) {
        return Err(invalid(
            "service executable changed during identity verification",
        ));
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
pub(crate) fn binary_digest_with_hook(
    path: &Path,
    after_open: impl FnOnce() -> io::Result<()>,
) -> io::Result<String> {
    binary_digest_with_hook_inner(path, after_open)
}

/// A single bounded launchctl call. Service-specific code interprets
/// `Refused`; an unknown outcome is never retried blindly.
pub(crate) mod launchd {
    #[cfg(target_os = "macos")]
    use std::io;

    #[cfg(any(target_os = "macos", test))]
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Observation {
        Success,
        Refused,
        Unknown,
    }

    /// Whether launchd has a label loaded, as `launchctl print` answers:
    /// exit 0 is loaded, exit 113 ("could not find service") is not loaded,
    /// and anything else (another exit, a timeout) proves nothing.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) enum LabelState {
        Loaded,
        NotLoaded,
        Unknown,
    }

    /// What one `launchctl print <domain>/<label>` answered: exit 0 is
    /// loaded, exit 113 ("could not find service") is not loaded, and any
    /// other exit, or no exit at all (`None`: a timeout or output limit),
    /// proves nothing.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn print_exit_state(exit: Option<i32>) -> LabelState {
        match exit {
            Some(0) => LabelState::Loaded,
            Some(113) => LabelState::NotLoaded,
            _ => LabelState::Unknown,
        }
    }

    /// One label's state from its answers in both per-user domains: loaded
    /// in either is loaded, not loaded needs both to say not found, and
    /// anything else is unknown.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn combine_domains(gui: LabelState, user: LabelState) -> LabelState {
        match (gui, user) {
            (LabelState::Loaded, _) | (_, LabelState::Loaded) => LabelState::Loaded,
            (LabelState::NotLoaded, LabelState::NotLoaded) => LabelState::NotLoaded,
            _ => LabelState::Unknown,
        }
    }

    /// Ask launchd whether `<label>` is loaded in `gui/<uid>` or
    /// `user/<uid>`: a job bootstrapped into either domain runs.
    #[cfg(target_os = "macos")]
    pub(crate) fn label_state(uid: u32, label: &str) -> io::Result<LabelState> {
        use crate::bounded_output::{LimitedOutput, output_limited};
        use std::process::Command;
        use std::time::Duration;
        let _span = crate::phase_telemetry::span("launchd_label_state");
        let ask = |domain: &str| -> io::Result<LabelState> {
            let output = output_limited(
                Command::new("/bin/launchctl").args(["print", &format!("{domain}/{uid}/{label}")]),
                Duration::from_secs(10),
                64 * 1024,
            )?;
            Ok(print_exit_state(match output {
                LimitedOutput::Completed(output) => output.status.code(),
                _ => None,
            }))
        };
        let gui = ask("gui")?;
        if gui == LabelState::Loaded {
            return Ok(gui);
        }
        Ok(combine_domains(gui, ask("user")?))
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn launchctl(args: &[String]) -> io::Result<Observation> {
        use crate::bounded_output::{LimitedOutput, output_limited};
        use std::process::Command;
        use std::time::Duration;
        let output = output_limited(
            Command::new("/bin/launchctl").args(args),
            Duration::from_secs(10),
            64 * 1024,
        )?;
        Ok(match output {
            LimitedOutput::Completed(output) if output.status.success() => Observation::Success,
            LimitedOutput::Completed(_) => Observation::Refused,
            LimitedOutput::Deadline | LimitedOutput::OutputLimit => Observation::Unknown,
        })
    }
}
