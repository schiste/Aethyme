//! Private subprocess protocol for bounded agent logs. This is transport, not
//! an authorization boundary: the operator already controls the agent command.
use std::io;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

const FLAG: &str = "--aethyme-agent-supervisor-v1";
#[cfg(unix)]
const CLEANUP_FLAG: &str = "--aethyme-agent-supervisor-cleanup-v1";
#[cfg(unix)]
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
pub(crate) const GO: &str = "AETHYME_START_AGENT_V1";

#[cfg(unix)]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Launch {
    executable: PathBuf,
    repository: PathBuf,
    common_dir: PathBuf,
    worktree: PathBuf,
    command: String,
    nonce: String,
    log_device: u64,
    log_inode: u64,
}

/// Dispatch the inline-exec, agent-start and agent-cleanup protocols before
/// normal CLI parsing, compatibility checks or broker opens. Unknown reserved
/// protocol versions fail closed rather than becoming commands.
pub fn dispatch(args: &[String]) -> Option<u8> {
    if let Some(code) = crate::inline_exec_supervisor::dispatch(args) {
        return Some(code);
    }
    if !args.first()?.starts_with("--aethyme-agent-supervisor") {
        return None;
    }
    if args.len() != 2 {
        return Some(125);
    }
    #[cfg(unix)]
    if args[0] == CLEANUP_FLAG {
        return Some(cleanup_watcher(&args[1]).unwrap_or(125));
    }
    if args[0] != FLAG {
        return Some(125);
    }
    #[cfg(unix)]
    return Some(run(&args[1]).unwrap_or(125));
    #[cfg(not(unix))]
    Some(125)
}

pub(crate) fn ensure_supported() -> io::Result<()> {
    if cfg!(unix) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "bounded agent supervision requires Unix",
        ))
    }
}

/// Resolve an exact local executable. The caller chooses its trust/version
/// policy; this private protocol never searches PATH or guesses an installation.
pub(crate) fn resolve_executable(explicit: Option<&Path>) -> io::Result<std::path::PathBuf> {
    ensure_supported()?;
    let executable = match explicit {
        Some(path) => path.to_path_buf(),
        None => std::env::current_exe()?,
    }
    .canonicalize()?;
    let metadata = executable.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "agent supervisor executable is not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "agent supervisor file is not executable",
            ));
        }
    }
    Ok(executable)
}

#[cfg(unix)]
pub(crate) fn process(
    command: &str,
    worktree: &Path,
    log: &std::fs::File,
    executable: Option<&Path>,
) -> io::Result<(std::process::Command, String)> {
    use std::os::unix::fs::MetadataExt as _;
    ensure_supported()?;
    if command.len() > 16 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "agent command exceeds 16 KiB",
        ));
    }
    let repo = crate::git::GitRepo::discover(worktree).map_err(io::Error::other)?;
    let metadata = log.metadata()?;
    let launch = Launch {
        executable: resolve_executable(executable)?,
        repository: repo.main_root().map_err(io::Error::other)?.canonicalize()?,
        common_dir: repo
            .git_common_dir()
            .map_err(io::Error::other)?
            .canonicalize()?,
        worktree: worktree.canonicalize()?,
        command: command.to_owned(),
        nonce: format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(io::Error::other)?
                .as_nanos()
        ),
        log_device: metadata.dev(),
        log_inode: metadata.ino(),
    };
    let payload = serde_json::to_string(&launch).map_err(io::Error::other)?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "agent launch payload exceeds 64 KiB",
        ));
    }
    let mut process = std::process::Command::new(&launch.executable);
    #[cfg(not(test))]
    process.arg(FLAG).arg(payload);
    #[cfg(test)]
    if executable.is_none() {
        process
            .args([
                "--exact",
                "agent_supervisor::tests::supervisor_child_fixture",
                "--nocapture",
                "--quiet",
            ])
            .env("AETHYME_TEST_AGENT_SUPERVISOR_PAYLOAD", payload);
    } else {
        process.arg(FLAG).arg(payload);
    }
    Ok((process, launch.nonce))
}

#[cfg(not(unix))]
pub(crate) fn process(
    _: &str,
    _: &Path,
    _: &std::fs::File,
    _: Option<&Path>,
) -> io::Result<(std::process::Command, String)> {
    ensure_supported()?;
    unreachable!()
}

#[cfg(unix)]
pub(crate) fn await_ready(stdout: &mut std::process::ChildStdout, nonce: &str) -> io::Result<()> {
    crate::gates::bounded_log::set_nonblocking(stdout)?;
    let expected = format!("AETHYME_AGENT_READY {nonce}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    await_ready_with_wait(stdout, &expected, deadline, || {
        std::thread::sleep(std::time::Duration::from_millis(5));
    })
}

#[cfg(unix)]
fn await_ready_with_wait<R: std::io::Read>(
    stdout: &mut R,
    expected: &str,
    deadline: std::time::Instant,
    mut wait: impl FnMut(),
) -> io::Result<()> {
    let mut line = Vec::new();
    let mut total = 0;
    loop {
        let mut byte = [0u8];
        match stdout.read(&mut byte) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "agent supervisor closed before readiness",
                ));
            }
            Ok(_) => {
                total += 1;
                if total > 4096 {
                    return Err(io::Error::other("oversized supervisor readiness response"));
                }
                if byte[0] == b'\n' {
                    if line == expected.as_bytes() {
                        return Ok(());
                    }
                    #[cfg(not(test))]
                    return Err(io::Error::other("invalid supervisor readiness response"));
                    #[cfg(test)]
                    line.clear(); // Rust's test harness emits a bounded preamble.
                } else {
                    line.push(byte[0]);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "agent supervisor readiness timed out",
                    ));
                }
                wait();
                continue;
            }
            Err(error) => return Err(error),
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "agent supervisor readiness timed out",
            ));
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn await_ready(_: &mut std::process::ChildStdout, _: &str) -> io::Result<()> {
    ensure_supported()
}

#[cfg(unix)]
fn run(payload: &str) -> io::Result<u8> {
    use crate::gates::bounded_log::{
        AGENT_LOG_MAX_BYTES, BoundedGateLog, PIPE_DRAIN_GRACE, drain_pipe, set_nonblocking,
    };
    use std::fs::File;
    use std::io::{Read as _, Write as _};
    use std::os::fd::FromRawFd as _;
    use std::os::unix::fs::MetadataExt as _;
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    // SAFETY: duplicate the inherited precreated log descriptor, then own only
    // that duplicate. No raw agent output or diagnostics go to stderr.
    let descriptor = unsafe { libc::fcntl(libc::STDERR_FILENO, libc::F_DUPFD_CLOEXEC, 3) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a fresh valid descriptor; this File assumes its
    // sole ownership and closes only that duplicate.
    let log = unsafe { File::from_raw_fd(descriptor) };
    // Diagnostics and panics must never append to the bounded log outside its
    // collector. The retained duplicate is CLOEXEC and is not passed to agents.
    use std::os::fd::AsRawFd as _;
    let null = File::open("/dev/null")?;
    // SAFETY: null owns a live descriptor and STDERR_FILENO is a valid dup2
    // target; no Rust handle owns the inherited stderr descriptor.
    if unsafe { libc::dup2(null.as_raw_fd(), libc::STDERR_FILENO) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // The launcher creates this group before exec. Without that binding a
    // private invocation must never signal an interactive or unrelated group.
    // SAFETY: these kernel identity queries take no arguments or pointers.
    let (group, pid) = unsafe { (libc::getpgrp(), libc::getpid()) };
    if group != pid {
        return Err(io::Error::other(
            "agent supervisor is not its owned group leader",
        ));
    }
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(io::Error::other("oversized agent launch payload"));
    }
    let launch: Launch = serde_json::from_str(payload).map_err(io::Error::other)?;
    if launch.command.len() > 16 * 1024
        || launch.nonce.is_empty()
        || launch.nonce.len() > 96
        || !launch
            .nonce
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'-')
    {
        return Err(io::Error::other("invalid agent launch fields"));
    }
    for path in [
        &launch.executable,
        &launch.repository,
        &launch.common_dir,
        &launch.worktree,
    ] {
        if !path.is_absolute() || path.canonicalize()? != *path {
            return Err(io::Error::other("noncanonical agent launch path"));
        }
    }
    if std::env::current_exe()?.canonicalize()? != launch.executable
        || std::env::current_dir()?.canonicalize()? != launch.worktree
    {
        return Err(io::Error::other("agent launch runtime or cwd mismatch"));
    }
    let checkout = crate::git::GitRepo::discover(&launch.worktree).map_err(io::Error::other)?;
    if checkout
        .main_root()
        .map_err(io::Error::other)?
        .canonicalize()?
        != launch.repository
        || checkout
            .git_common_dir()
            .map_err(io::Error::other)?
            .canonicalize()?
            != launch.common_dir
    {
        return Err(io::Error::other("agent launch repository binding mismatch"));
    }
    let repository = crate::git::GitRepo::discover(&launch.repository).map_err(io::Error::other)?;
    // Validate the inherited private capsule before any broker open or command.
    crate::broker::BrokerStorageContext::capture_for_repository(&repository)
        .map_err(io::Error::other)?;
    let metadata = log.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.dev() != launch.log_device
        || metadata.ino() != launch.log_inode
    {
        return Err(io::Error::other("agent log descriptor identity mismatch"));
    }
    let mut collector = BoundedGateLog::from_file(log, AGENT_LOG_MAX_BYTES, "agent")?;
    collector.write_header(b"aethyme: agent capture initially INCOMPLETE; only a final COMPLETE footer confirms capture\n")?;
    collector.sync_data()?;
    let mut cleanup = start_cleanup_watcher()?;
    let mut ready = io::stdout().lock();
    writeln!(ready, "AETHYME_AGENT_READY {}", launch.nonce)?;
    ready.flush()?;
    drop(ready);
    let mut token = Vec::new();
    io::stdin().lock().take(128).read_to_end(&mut token)?;
    if token != format!("{GO}\n").as_bytes() {
        return Ok(125);
    }

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&launch.command)
        .current_dir(&launch.worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Keep the shell in the supervisor's process group so the broker owns one
    // exact group. This does not contain descendants that deliberately setsid.
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing stderr"))?;
    if let Err(error) = set_nonblocking(&stdout).and_then(|()| set_nonblocking(&stderr)) {
        stop_direct_child(&mut child);
        return Err(error);
    }
    let collector = Arc::new(Mutex::new(collector));
    let stop = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let first = Arc::new(AtomicI64::new(-1));
    let error = Arc::new(Mutex::new(None));
    let started = Instant::now();
    #[cfg(test)]
    let inject_stdout_failure = std::env::var_os("AETHYME_TEST_AGENT_DRAIN_FAILURE").is_some();
    macro_rules! drainer {
        ($pipe:expr, $inject:expr) => {{
            let (c, s, f, t, e) = (
                collector.clone(),
                stop.clone(),
                failed.clone(),
                first.clone(),
                error.clone(),
            );
            std::thread::Builder::new()
                .name("aethyme-agent-log".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        #[cfg(test)]
                        if inject_stdout_failure && $inject {
                            return Err(io::Error::other("injected agent pipe read failure"));
                        }
                        drain_pipe($pipe, c, s, t, f.clone(), e.clone(), started)
                    }))
                    .unwrap_or_else(|_| Err(io::Error::other("agent drainer panicked")));
                    if let Err(error) = &result {
                        f.store(true, Ordering::Release);
                        let mut slot = e.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        if slot.is_none() {
                            *slot = Some(error.to_string().chars().take(512).collect());
                        }
                    }
                    result
                })
        }};
    }
    let out = match drainer!(stdout, true) {
        Ok(handle) => handle,
        Err(error) => {
            stop_direct_child(&mut child);
            return Err(error);
        }
    };
    let err = match drainer!(stderr, false) {
        Ok(handle) => handle,
        Err(error) => {
            stop.store(true, Ordering::Release);
            stop_direct_child(&mut child);
            crate::warn_unrecorded(
                "join first agent collector",
                out.join()
                    .map_err(|_| io::Error::other("collector panic"))
                    .and_then(|result| result),
            );
            return Err(error);
        }
    };
    // This subprocess is the sole waiter for the shell. It survives launcher
    // exit and never signals a reaped PID or a process-group id after reaping.
    let status = loop {
        if !matches!(cleanup.try_wait(), Ok(None)) {
            let mut log = collector
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            log.write_note(
                b"\naethyme: cleanup watcher exited before agent completion; capture INCOMPLETE\n",
            );
            crate::warn_unrecorded("persist cleanup watcher failure", log.sync_data());
            // This process remains the original group leader. A failed watcher
            // must not turn into permission to leave its command running.
            // SAFETY: this live, verified group leader pins its own group id;
            // SIGKILL targets only that owned group, including this process.
            unsafe {
                libc::killpg(libc::getpid(), libc::SIGKILL);
            }
            std::process::exit(125);
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Err(error) => break Err(error),
            Ok(None) if failed.load(Ordering::Acquire) => {
                break Err(io::Error::other(
                    "agent pipe capture failed while command was running",
                ));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    };
    let deadline = Instant::now() + PIPE_DRAIN_GRACE;
    while !(out.is_finished() && err.is_finished()) && Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let held_pipes = !(out.is_finished() && err.is_finished());
    stop.store(true, Ordering::Release);
    let out_ok = matches!(out.join(), Ok(Ok(())));
    let err_ok = matches!(err.join(), Ok(Ok(())));
    let mut collector = collector
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(detail) = error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_deref()
    {
        let detail = detail.chars().take(512).collect::<String>();
        collector.write_note(format!("\naethyme: agent pipe capture error: {detail}\n").as_bytes());
    }
    let cleanup_ready = matches!(cleanup.try_wait(), Ok(None));
    let summary = collector.finish();
    let complete = cleanup_ready
        && status.is_ok()
        && !held_pipes
        && out_ok
        && err_ok
        && !failed.load(Ordering::Acquire)
        && !summary.incomplete;
    collector.append_note_after_finish(if complete {
        b"\naethyme: agent capture COMPLETE\n"
    } else {
        b"\naethyme: agent capture INCOMPLETE (pipe, process or storage failure)\n"
    });
    collector.sync_data()?;
    if !cleanup_ready {
        // SAFETY: this live, verified group leader still pins its own group id.
        // Killing that group includes this process and its owned descendants.
        unsafe {
            libc::killpg(libc::getpid(), libc::SIGKILL);
        }
        std::process::exit(125);
    }
    let status = status?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
        .clamp(0, 255) as u8)
}

/// Stop only an unreaped child owned by this process, with a bounded reap poll.
/// Whole-group cleanup belongs to the still-live watcher after supervisor exit.
#[cfg(unix)]
fn stop_direct_child(child: &mut std::process::Child) {
    use std::time::{Duration, Instant};
    match child.try_wait() {
        Ok(Some(_)) => return,
        Err(error) => {
            crate::warn_unrecorded("inspect owned child before stop", Err::<(), _>(error));
            return;
        }
        Ok(None) => {}
    }
    crate::warn_unrecorded("stop unreaped agent child", child.kill());
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Err(error) => {
                crate::warn_unrecorded("poll owned child reap", Err::<(), _>(error));
                return;
            }
            Ok(None) if Instant::now() >= deadline => {
                crate::warn_unrecorded(
                    "owned child reap remains incomplete",
                    Err::<(), _>(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "child did not reap before cleanup deadline",
                    )),
                );
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

#[cfg(unix)]
fn cleanup_watcher(expected: &str) -> io::Result<u8> {
    use std::io::Write as _;
    let parent: i32 = expected.parse().map_err(io::Error::other)?;
    // SAFETY: these kernel identity queries take no arguments or pointers.
    let (actual_parent, group, pid) = unsafe { (libc::getppid(), libc::getpgrp(), libc::getpid()) };
    if parent <= 1 || actual_parent != parent || group != parent || pid == parent {
        return Err(io::Error::other("cleanup watcher ownership mismatch"));
    }
    let mut ready = io::stdout().lock();
    writeln!(ready, "AETHYME_AGENT_READY cleanup-{parent}")?;
    ready.flush()?;
    drop(ready);
    // SAFETY: getppid is a read-only kernel identity query without arguments.
    while unsafe { libc::getppid() } == parent {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    #[cfg(test)]
    if let Some(path) = std::env::var_os("AETHYME_TEST_AGENT_CLEANUP_HOLD") {
        let path = std::path::PathBuf::from(path);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    // This still-live member pins the original PGID after the supervisor dies.
    // It kills itself with any remaining ordinary descendants, so no later
    // signal can accidentally target a recycled process-group id.
    // SAFETY: getpgrp is a read-only kernel identity query without arguments.
    if unsafe { libc::getpgrp() } != parent {
        return Err(io::Error::other("cleanup watcher group changed"));
    }
    // SAFETY: this still-live watcher belongs to and pins the verified group;
    // SIGKILL includes itself, so no signal can follow group-id reuse.
    if unsafe { libc::killpg(parent, libc::SIGKILL) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(125) // SIGKILL includes this watcher; reaching here is nonconclusive.
}

#[cfg(unix)]
pub(crate) fn start_cleanup_watcher_for_inline() -> io::Result<std::process::Child> {
    start_cleanup_watcher()
}

#[cfg(unix)]
fn start_cleanup_watcher() -> io::Result<std::process::Child> {
    use std::process::{Command, Stdio};
    let parent = std::process::id();
    let mut process = Command::new(std::env::current_exe()?);
    #[cfg(not(test))]
    process.arg(CLEANUP_FLAG).arg(parent.to_string());
    #[cfg(test)]
    process
        .args([
            "--exact",
            "agent_supervisor::tests::cleanup_child_fixture",
            "--nocapture",
            "--quiet",
        ])
        .env("AETHYME_TEST_AGENT_CLEANUP_PARENT", parent.to_string());
    // Deliberately inherit the supervisor group, not process_group(0).
    let mut child = process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let readiness = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing cleanup readiness pipe"))
        .and_then(|mut stdout| await_ready(&mut stdout, &format!("cleanup-{parent}")));
    if let Err(error) = readiness {
        stop_direct_child(&mut child);
        return Err(error);
    }
    Ok(child)
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn supervisor_child_fixture() {
        let Ok(payload) = std::env::var("AETHYME_TEST_AGENT_SUPERVISOR_PAYLOAD") else {
            return;
        };
        std::process::exit(super::run(&payload).unwrap_or(125).into());
    }

    #[test]
    fn explicit_executable_is_exact_and_rejects_missing_directory_or_nonexecutable() {
        use std::os::unix::fs::PermissionsExt as _;
        let owner = tempfile::tempdir().unwrap();
        let absent = owner.path().join("missing");
        assert!(super::resolve_executable(Some(&absent)).is_err());
        assert!(super::resolve_executable(Some(owner.path())).is_err());
        let file = owner.path().join("supervisor");
        std::fs::write(&file, b"unused executable fixture").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(super::resolve_executable(Some(&file)).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = owner.path().join("selected");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(
            super::resolve_executable(Some(&link)).unwrap(),
            file.canonicalize().unwrap()
        );
        assert_eq!(
            super::resolve_executable(None).unwrap(),
            std::env::current_exe().unwrap().canonicalize().unwrap()
        );
    }

    #[test]
    fn reserved_protocol_never_routes_malformed_invocations() {
        assert_eq!(
            super::dispatch(&["--aethyme-agent-supervisor-v9".into()]),
            Some(125)
        );
        assert_eq!(super::dispatch(&[super::FLAG.into()]), Some(125));
        assert_eq!(
            super::dispatch(&[super::FLAG.into(), "{}".into()]),
            Some(125)
        );
        assert_eq!(super::dispatch(&["status".into()]), None);
    }

    #[test]
    fn cleanup_child_fixture() {
        let Ok(parent) = std::env::var("AETHYME_TEST_AGENT_CLEANUP_PARENT") else {
            return;
        };
        std::process::exit(super::cleanup_watcher(&parent).unwrap_or(125).into());
    }

    #[test]
    fn readiness_does_not_wait_between_already_available_bytes() {
        struct OneByteReader {
            bytes: Vec<u8>,
            offset: usize,
        }

        impl std::io::Read for OneByteReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.offset == self.bytes.len() {
                    return Ok(0);
                }
                buffer[0] = self.bytes[self.offset];
                self.offset += 1;
                Ok(1)
            }
        }

        let expected = "AETHYME_AGENT_READY cleanup-burst";
        let mut reader = OneByteReader {
            bytes: format!("{expected}\n").into_bytes(),
            offset: 0,
        };
        let mut waits = 0;

        super::await_ready_with_wait(
            &mut reader,
            expected,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
            || waits += 1,
        )
        .unwrap();

        assert_eq!(waits, 0, "readiness bytes already available must not sleep");
    }

    #[test]
    fn readiness_waits_only_after_would_block_then_reads_ready_frame() {
        struct WouldBlockOnceReader {
            block_once: bool,
            bytes: Vec<u8>,
            offset: usize,
        }

        impl std::io::Read for WouldBlockOnceReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.block_once {
                    self.block_once = false;
                    return Err(std::io::ErrorKind::WouldBlock.into());
                }
                if self.offset == self.bytes.len() {
                    return Ok(0);
                }
                buffer[0] = self.bytes[self.offset];
                self.offset += 1;
                Ok(1)
            }
        }

        let expected = "AETHYME_AGENT_READY cleanup-after-would-block";
        let mut reader = WouldBlockOnceReader {
            block_once: true,
            bytes: format!("{expected}\n").into_bytes(),
            offset: 0,
        };
        let mut waits = 0;

        super::await_ready_with_wait(
            &mut reader,
            expected,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
            || waits += 1,
        )
        .unwrap();

        assert_eq!(waits, 1, "only the initial WouldBlock should wait");
    }

    #[test]
    fn readiness_expired_deadline_does_not_wait_after_would_block() {
        struct WouldBlockReader;

        impl std::io::Read for WouldBlockReader {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::WouldBlock.into())
            }
        }

        let mut reader = WouldBlockReader;
        let mut waits = 0;
        let error = super::await_ready_with_wait(
            &mut reader,
            "AETHYME_AGENT_READY cleanup-expired",
            std::time::Instant::now() - std::time::Duration::from_secs(1),
            || waits += 1,
        )
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert_eq!(waits, 0, "an expired deadline must not invoke the wait");
    }

    fn initialized_repo(path: &std::path::Path) {
        std::fs::create_dir_all(path).unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "initial",
            ],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(path)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    fn spawn(command: &str, repo: &std::path::Path, log: &std::path::Path) -> std::process::Child {
        spawn_configured(command, repo, log, false, None)
    }

    fn spawn_configured(
        command: &str,
        repo: &std::path::Path,
        log: &std::path::Path,
        inject_failure: bool,
        cleanup_hold: Option<&std::path::Path>,
    ) -> std::process::Child {
        use std::os::unix::process::CommandExt as _;
        let log = std::fs::File::create(log).unwrap();
        let (mut process, nonce) = super::process(command, repo, &log, None).unwrap();
        process
            .current_dir(repo)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(log)
            .env_remove("AETHYME_BROKER_STORAGE_CONTEXT_V1")
            .process_group(0);
        process.env_remove("AETHYME_TEST_AGENT_CLEANUP_HOLD");
        if let Some(path) = cleanup_hold {
            process.env("AETHYME_TEST_AGENT_CLEANUP_HOLD", path);
        }
        process.env_remove("AETHYME_TEST_AGENT_DRAIN_FAILURE");
        if inject_failure {
            process.env("AETHYME_TEST_AGENT_DRAIN_FAILURE", "1");
        }
        let mut child = process.spawn().unwrap();
        let result = super::await_ready(child.stdout.as_mut().unwrap(), &nonce);
        if let Err(error) = result {
            crate::warn_unrecorded("stop failed fixture supervisor", child.kill());
            crate::warn_unrecorded("reap failed fixture supervisor", child.wait());
            panic!("fixture supervisor readiness: {error}");
        }
        child.stdout.take();
        child
    }

    fn release(child: &mut std::process::Child) {
        use std::io::Write as _;
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{}", super::GO).unwrap();
    }

    fn wait(child: &mut std::process::Child) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            if std::time::Instant::now() > deadline {
                // SAFETY: this test owns the live, unreaped group leader.
                unsafe {
                    libc::killpg(child.id() as i32, libc::SIGKILL);
                }
                crate::warn_unrecorded("reap timed-out fixture supervisor", child.wait());
                panic!("fixture supervisor timed out");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn flood_from_both_streams_is_capped_and_keeps_tail_and_exit_code() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        initialized_repo(&repo);
        let log = temp.path().join("agent.log");
        let mut child = spawn(
            "dd if=/dev/zero bs=1048576 count=20 2>/dev/null; dd if=/dev/zero bs=1048576 count=20 1>&2 2>/dev/null; printf FINAL-AGENT-TAIL; exit 7",
            &repo,
            &log,
        );
        let initial = std::fs::read(&log).unwrap();
        assert!(
            initial
                .windows(b"initially INCOMPLETE".len())
                .any(|w| w == b"initially INCOMPLETE")
        );
        release(&mut child);
        assert_eq!(wait(&mut child).code(), Some(7));
        let bytes = std::fs::read(&log).unwrap();
        assert!(bytes.len() as u64 <= crate::gates::bounded_log::AGENT_LOG_MAX_BYTES);
        assert!(
            bytes
                .windows(b"agent output capture truncated".len())
                .any(|w| w == b"agent output capture truncated")
        );
        assert!(
            bytes
                .windows(b"FINAL-AGENT-TAIL".len())
                .any(|w| w == b"FINAL-AGENT-TAIL")
        );
        assert!(
            bytes
                .windows(b"observed 41943056 bytes".len())
                .any(|w| w == b"observed 41943056 bytes"),
            "both complete 20 MiB streams must be drained"
        );
        assert!(bytes.ends_with(b"aethyme: agent capture COMPLETE\n"));
    }

    #[test]
    fn eof_before_go_never_executes_user_command() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        initialized_repo(&repo);
        let log = temp.path().join("agent.log");
        let mut child = spawn("printf forbidden > must-not-run", &repo, &log);
        child.stdin.take();
        assert_eq!(wait(&mut child).code(), Some(125));
        assert!(!repo.join("must-not-run").exists());
        assert!(
            !std::fs::read(&log)
                .unwrap()
                .ends_with(b"aethyme: agent capture COMPLETE\n")
        );
    }

    #[test]
    fn killed_supervisor_retains_initial_incomplete_record() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        initialized_repo(&repo);
        let log = temp.path().join("agent.log");
        let mut child = spawn("printf forbidden > must-not-run", &repo, &log);
        child.kill().unwrap();
        wait(&mut child);
        assert!(!repo.join("must-not-run").exists());
        let bytes = std::fs::read(&log).unwrap();
        assert!(
            bytes
                .windows(b"initially INCOMPLETE".len())
                .any(|w| w == b"initially INCOMPLETE")
        );
        assert!(!bytes.ends_with(b"aethyme: agent capture COMPLETE\n"));
    }

    #[test]
    fn descendant_held_pipes_finish_with_incomplete_capture() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        initialized_repo(&repo);
        let log = temp.path().join("agent.log");
        let mut child = spawn("sleep 4 & printf leader-finished", &repo, &log);
        release(&mut child);
        assert!(wait(&mut child).success());
        let bytes = std::fs::read(&log).unwrap();
        assert!(
            bytes.ends_with(
                b"aethyme: agent capture INCOMPLETE (pipe, process or storage failure)\n"
            )
        );
    }

    #[test]
    fn launcher_child_fixture() {
        let Ok(root) = std::env::var("AETHYME_TEST_AGENT_LAUNCHER_ROOT") else {
            return;
        };
        let root = std::path::Path::new(&root);
        let mut child = spawn(
            "sleep 1; printf AFTER-LAUNCHER-EXIT",
            &root.join("repo"),
            &root.join("agent.log"),
        );
        release(&mut child);
        // Deliberate parent process exit: no collector lives in this process.
        std::process::exit(0);
    }

    #[test]
    fn capture_continues_after_launcher_process_exit() {
        let temp = tempfile::tempdir().unwrap();
        initialized_repo(&temp.path().join("repo"));
        assert!(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "agent_supervisor::tests::launcher_child_fixture",
                    "--nocapture",
                    "--quiet"
                ])
                .env("AETHYME_TEST_AGENT_LAUNCHER_ROOT", temp.path())
                .status()
                .unwrap()
                .success()
        );
        let log = temp.path().join("agent.log");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let bytes = std::fs::read(&log).unwrap();
            if bytes.ends_with(b"aethyme: agent capture COMPLETE\n") {
                assert!(
                    bytes
                        .windows(b"AFTER-LAUNCHER-EXIT".len())
                        .any(|w| w == b"AFTER-LAUNCHER-EXIT")
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "detached capture did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn assert_writer_stops(repo: &std::path::Path) {
        // The cleanup watcher runs after the tracked supervisor's actual death.
        // Allow its cooperative scheduling interval, then prove no more writes.
        std::thread::sleep(std::time::Duration::from_millis(250));
        let path = repo.join("background-writes");
        let before = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let after = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        assert_eq!(
            before, after,
            "same-group descendant kept writing after cleanup"
        );
    }

    #[test]
    fn closed_pipe_background_writer_stops_after_supervisor_exit() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        initialized_repo(&repo);
        // A failed cleanup guard must still leave a finite, fixture-owned
        // writer: teardown removes its stop sentinel on panic. The write cap
        // also bounds output if the test runner itself is killed.
        std::fs::write(repo.join("writer-alive"), b"fixture-owned writer lifetime").unwrap();
        let log = temp.path().join("agent.log");
        let mut child = spawn(
            "sh -c 'n=0; while [ -f writer-alive ] && [ $n -lt 3000 ]; do printf x >> background-writes; n=$((n + 1)); sleep 0.02; done' >/dev/null 2>&1 & sleep 0.15; printf foreground-done",
            &repo,
            &log,
        );
        release(&mut child);
        assert!(wait(&mut child).success());
        assert!(
            std::fs::metadata(repo.join("background-writes"))
                .unwrap()
                .len()
                > 0
        );
        assert_writer_stops(&repo);
    }

    #[test]
    fn drainer_failure_stops_running_command_and_its_background_writer() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        initialized_repo(&repo);
        // A failed cleanup guard must still leave a finite, fixture-owned
        // writer: teardown removes its stop sentinel on panic. The write cap
        // also bounds output if the test runner itself is killed.
        std::fs::write(repo.join("writer-alive"), b"fixture-owned writer lifetime").unwrap();
        let log = temp.path().join("agent.log");
        let mut child = spawn_configured(
            "n=0; while [ -f writer-alive ] && [ $n -lt 3000 ]; do printf x >> background-writes; n=$((n + 1)); sleep 0.02; done",
            &repo,
            &log,
            true,
            None,
        );
        let started = std::time::Instant::now();
        release(&mut child);
        assert_eq!(wait(&mut child).code(), Some(125));
        assert!(started.elapsed() < std::time::Duration::from_secs(8));
        let bytes = std::fs::read(&log).unwrap();
        assert!(
            bytes.ends_with(
                b"aethyme: agent capture INCOMPLETE (pipe, process or storage failure)\n"
            )
        );
        assert_writer_stops(&repo);
    }

    #[test]
    fn session_remains_live_until_the_cleanup_group_is_gone() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        initialized_repo(&repo);
        // A failed cleanup guard must still leave a finite, fixture-owned
        // writer: teardown removes its stop sentinel on panic. The write cap
        // also bounds output if the test runner itself is killed.
        std::fs::write(repo.join("writer-alive"), b"fixture-owned writer lifetime").unwrap();
        let log = temp.path().join("agent.log");
        let hold = temp.path().join("hold-cleanup");
        std::fs::write(&hold, b"test-owned barrier").unwrap();
        let mut child = spawn_configured(
            "sh -c 'n=0; while [ -f writer-alive ] && [ $n -lt 3000 ]; do printf x >> background-writes; n=$((n + 1)); sleep 0.02; done' >/dev/null 2>&1 & sleep 0.15",
            &repo,
            &log,
            false,
            Some(&hold),
        );
        let group = child.id() as i64;
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repo).unwrap();
        let session = broker
            .store()
            .register_session(&crate::types::NewSession {
                worktree_path: repo.to_string_lossy().into_owned(),
                branch: "main".into(),
                origin: crate::types::SessionOrigin::Spawned,
                task: Some("supervisor cleanup liveness".into()),
                diff_base: None,
                adoption_base: None,
                adopted_head: None,
                repository_contract: None,
                pid: Some(group),
                command: Some("test-owned background writer".into()),
                log_path: Some(log.to_string_lossy().into_owned()),
                agent_identity: None,
            })
            .unwrap();
        release(&mut child);
        assert!(wait(&mut child).success());
        assert!(
            !crate::broker::pid_alive(group),
            "tracked supervisor was reaped"
        );
        assert!(
            crate::broker::session_pid_alive(group),
            "cleanup watcher and descendants must protect the session"
        );
        // Exercise both production session consumers while only the cleanup
        // watcher and writer remain. Neither may grant cleanup candidacy.
        for views in [
            broker.agents_snapshot(i64::MAX).unwrap(),
            broker.agents(i64::MAX).unwrap(),
        ] {
            let view = views
                .iter()
                .find(|view| view.session.id == session.id)
                .unwrap();
            assert_eq!(view.pid_alive, Some(true));
            assert!(!view.derived_status.is_closed());
        }
        assert!(
            !broker
                .store()
                .session(session.id)
                .unwrap()
                .status
                .is_closed()
        );
        let before = std::fs::metadata(repo.join("background-writes"))
            .unwrap()
            .len();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let writer_progressed = loop {
            if std::time::Instant::now() >= deadline {
                break false;
            }
            if std::fs::metadata(repo.join("background-writes"))
                .unwrap()
                .len()
                > before
            {
                break std::time::Instant::now() < deadline;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(
            writer_progressed,
            "test must exercise a writer after supervisor exit"
        );
        std::fs::remove_file(&hold).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while crate::broker::session_pid_alive(group) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !crate::broker::session_pid_alive(group),
            "group should become reclaimable only after all members exit"
        );
        let views = broker.agents(i64::MAX).unwrap();
        let view = views
            .iter()
            .find(|view| view.session.id == session.id)
            .unwrap();
        assert_eq!(view.pid_alive, Some(false));
        assert!(view.derived_status.is_closed());
        assert_writer_stops(&repo);
    }
    #[test]
    fn invalid_session_ownership_stays_protected_without_group_zero_probe() {
        for pid in [0, -1, i64::from(i32::MAX) + 1] {
            assert!(crate::broker::session_pid_alive(pid));
        }
    }
}
