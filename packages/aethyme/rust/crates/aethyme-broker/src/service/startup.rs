//! Explicit opt-in startup. Neither discovery nor ordinary requests call this.

use super::{
    discovery::{self, ServiceDirectory},
    job_protocol::{Method, Reply},
    protocol::Pong,
    transport,
};
use crate::file_lock::ExclusiveFileLock;
use serde::Serialize;
use std::fs::{self, DirBuilder, Metadata};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(25);
const LOG_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct EnsureReport {
    #[serde(flatten)]
    pub location: ServiceDirectory,
    pub pid: u32,
    pub version: String,
    /// True only when this invocation started the observed service process.
    pub started: bool,
}

fn same_inode(first: &Metadata, second: &Metadata) -> bool {
    (first.dev(), first.ino()) == (second.dev(), second.ino())
}

fn changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "service startup paths changed",
    )
}

/// Only a missing or refused socket establishes absence. An incompatible,
/// unhealthy or unresponsive live endpoint is preserved and reported.
fn probe(location: &ServiceDirectory) -> io::Result<Option<Pong>> {
    let health = match transport::ping(&location.directory) {
        Ok(response) => response
            .result
            .map_err(|error| io::Error::other(error.message))?,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if health.pid == 0 || health.version != env!("CARGO_PKG_VERSION") {
        return Err(io::Error::other(
            "service build version differs; stop or upgrade it explicitly",
        ));
    }
    let reply = transport::scoped_job_request(
        &location.directory,
        &location.repository.key,
        Method::Repository {},
    )?
    .result
    .map_err(|error| io::Error::other(error.message))?;
    if reply
        != (Reply::Repository {
            identity: location.repository.clone(),
        })
    {
        return Err(io::Error::other("service repository owner differs"));
    }
    Ok(Some(health))
}

struct StartingChild(Option<Child>);

impl StartingChild {
    fn detach(self) -> io::Result<()> {
        // If thread creation fails, dropping the closure also drops the owned
        // startup guard, stopping and reaping the child rather than leaking it.
        drop(
            thread::Builder::new()
                .name("broker-service-reaper".into())
                .spawn(move || {
                    let mut owned = self;
                    if let Some(child) = &mut owned.0 {
                        crate::warn_unrecorded("reap broker service", child.wait());
                    }
                })?,
        );
        Ok(())
    }
}

impl Drop for StartingChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) => {
                    crate::warn_unrecorded("stop failed broker service startup", child.kill());
                    crate::warn_unrecorded("reap failed broker service startup", child.wait());
                }
                Err(error) => crate::warn_unrecorded::<(), _>(
                    "inspect failed broker service startup",
                    Err(error),
                ),
            }
        }
    }
}

/// Start the explicitly supplied executable, never a PATH lookup. The CLI
/// supplies its own current executable. The caller must keep the root private.
pub fn ensure(root: &Path, repository: &Path, executable: &Path) -> io::Result<EnsureReport> {
    let root_identity = transport::private_directory(root)?;
    let location = discovery::locate(root, repository)?;
    match DirBuilder::new().mode(0o700).create(&location.directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let directory_identity = transport::private_directory(&location.directory)?;
    let lock_path = location.directory.join("broker-service-startup.lock");
    let file = transport::private_coordination_file(&lock_path)?;
    let lock_identity = file.metadata()?;
    let validate = || -> io::Result<()> {
        if !same_inode(&root_identity, &transport::private_directory(root)?)
            || !same_inode(
                &directory_identity,
                &transport::private_directory(&location.directory)?,
            )
            || !same_inode(&lock_identity, &fs::symlink_metadata(&lock_path)?)
        {
            return Err(changed());
        }
        Ok(())
    };
    let lock_deadline = Instant::now() + WAIT;
    let _claim = loop {
        validate()?;
        if let Some(claim) = ExclusiveFileLock::try_acquire(file.try_clone()?)? {
            break claim;
        }
        if Instant::now() >= lock_deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "service startup claim is busy; retry explicitly",
            ));
        }
        thread::sleep(POLL);
    };
    validate()?;
    if let Some(health) = probe(&location)? {
        validate()?;
        return Ok(EnsureReport {
            location,
            pid: health.pid,
            version: health.version,
            started: false,
        });
    }
    let executable = executable.canonicalize()?;
    if !executable.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "service executable is not a file",
        ));
    }
    let log = transport::private_coordination_file(
        &location.directory.join("broker-service-startup.log"),
    )?;
    if log.metadata()?.len() > LOG_LIMIT {
        return Err(io::Error::other(
            "service startup log exceeds 8 MiB; rotate it explicitly before retrying",
        ));
    }
    // Append through the already-verified descriptor; never reopen its pathname.
    use std::io::{Seek, SeekFrom};
    let mut log = log;
    log.seek(SeekFrom::End(0))?;
    validate()?;
    let child = Command::new(executable)
        .args(["broker", "advanced", "service", "serve"])
        .arg(&location.directory)
        .arg("--jobs-for")
        .arg(&location.repository.root)
        .current_dir(&location.repository.root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .process_group(0)
        .spawn()?;
    let pid = child.id();
    let mut child = StartingChild(Some(child));
    let deadline = Instant::now() + WAIT;
    loop {
        validate()?;
        match probe(&location) {
            Ok(Some(health)) => {
                validate()?;
                let started = health.pid == pid;
                if started {
                    child.detach()?;
                }
                return Ok(EnsureReport {
                    location,
                    pid: health.pid,
                    version: health.version,
                    started,
                });
            }
            Ok(None) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => return Err(error),
        }
        if let Some(status) = child.0.as_mut().expect("starting child").try_wait()? {
            return Err(io::Error::other(format!(
                "broker service exited during startup ({status}); see {}",
                location
                    .directory
                    .join("broker-service-startup.log")
                    .display()
            )));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "broker service readiness expired; owned child stopped; inspect startup log before retrying",
            ));
        }
        thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_startup_guard_stops_and_reaps_its_owned_child() {
        let child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id() as libc::pid_t;
        drop(StartingChild(Some(child)));
        let mut status = 0;
        // SAFETY: wait only for this test's exact child, without blocking. An
        // unreaped live child yields 0; an unreaped exited child yields its pid.
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}
