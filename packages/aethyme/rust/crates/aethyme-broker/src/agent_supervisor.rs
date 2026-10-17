//! Private subprocess protocol for bounded agent logs. This is transport, not
//! an authorization boundary: the operator already controls the agent command.
use std::io;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

const FLAG: &str = "--aethyme-agent-supervisor-v1";
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

/// Dispatch before normal CLI parsing, compatibility checks or broker opens.
/// Unknown reserved protocol versions fail closed rather than becoming commands.
pub fn dispatch(args: &[String]) -> Option<u8> {
    if !args.first()?.starts_with("--aethyme-agent-supervisor") {
        return None;
    }
    if args.len() != 2 || args[0] != FLAG {
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

#[cfg(unix)]
pub(crate) fn process(
    command: &str,
    worktree: &Path,
    log: &std::fs::File,
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
        executable: std::env::current_exe()?.canonicalize()?,
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
    process
        .args([
            "--exact",
            "agent_supervisor::tests::supervisor_child_fixture",
            "--nocapture",
            "--quiet",
        ])
        .env("AETHYME_TEST_AGENT_SUPERVISOR_PAYLOAD", payload);
    Ok((process, launch.nonce))
}

#[cfg(not(unix))]
pub(crate) fn process(
    _: &str,
    _: &Path,
    _: &std::fs::File,
) -> io::Result<(std::process::Command, String)> {
    ensure_supported()?;
    unreachable!()
}

#[cfg(unix)]
pub(crate) fn await_ready(stdout: &mut std::process::ChildStdout, nonce: &str) -> io::Result<()> {
    use std::io::Read as _;
    crate::gates::bounded_log::set_nonblocking(stdout)?;
    let expected = format!("AETHYME_AGENT_READY {nonce}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
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
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "agent supervisor readiness timed out",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
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
    let log = unsafe { File::from_raw_fd(descriptor) };
    // Diagnostics and panics must never append to the bounded log outside its
    // collector. The retained duplicate is CLOEXEC and is not passed to agents.
    use std::os::fd::AsRawFd as _;
    let null = File::open("/dev/null")?;
    if unsafe { libc::dup2(null.as_raw_fd(), libc::STDERR_FILENO) } < 0 {
        return Err(io::Error::last_os_error());
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
        crate::warn_unrecorded("stop agent after pipe setup failure", child.kill());
        crate::warn_unrecorded("reap agent after pipe setup failure", child.wait());
        return Err(error);
    }
    let collector = Arc::new(Mutex::new(collector));
    let stop = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let first = Arc::new(AtomicI64::new(-1));
    let error = Arc::new(Mutex::new(None));
    let started = Instant::now();
    macro_rules! drainer {
        ($pipe:expr) => {{
            let (c, s, f, t, e) = (
                collector.clone(),
                stop.clone(),
                failed.clone(),
                first.clone(),
                error.clone(),
            );
            std::thread::Builder::new()
                .name("aethyme-agent-log".into())
                .spawn(move || drain_pipe($pipe, c, s, t, f, e, started))
        }};
    }
    let out = match drainer!(stdout) {
        Ok(handle) => handle,
        Err(error) => {
            crate::warn_unrecorded("stop agent after collector spawn failure", child.kill());
            crate::warn_unrecorded("reap agent after collector spawn failure", child.wait());
            return Err(error);
        }
    };
    let err = match drainer!(stderr) {
        Ok(handle) => handle,
        Err(error) => {
            stop.store(true, Ordering::Release);
            crate::warn_unrecorded("stop agent after collector spawn failure", child.kill());
            crate::warn_unrecorded("reap agent after collector spawn failure", child.wait());
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
    let status = child.wait();
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
    let summary = collector.finish();
    let complete = status.is_ok()
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
    let status = status?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(1))
        .clamp(0, 255) as u8)
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
        use std::os::unix::process::CommandExt as _;
        let log = std::fs::File::create(log).unwrap();
        let (mut process, nonce) = super::process(command, repo, &log).unwrap();
        process
            .current_dir(repo)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(log)
            .env_remove("AETHYME_BROKER_STORAGE_CONTEXT_V1")
            .process_group(0);
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
            "dd if=/dev/zero bs=1048576 count=20 2>/dev/null; dd if=/dev/zero bs=1048576 count=20 2>/dev/null 1>&2; printf FINAL-AGENT-TAIL; exit 7",
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
}
