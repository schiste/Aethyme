//! Run a child process to completion within a wall-clock budget.
//!
//! `Command::output` has no deadline, so a wedged child (a git waiting on an
//! unreachable remote, a lock held by a dead process) hangs its caller for as
//! long as the child lives (#219). This is the generic bounded variant. The
//! admission lane in `operations.rs` has its own `output_within`, which is
//! bound to that lane's error type and heartbeat; it can move onto this helper
//! in a follow-up.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long to wait between exit checks while the child still holds its
/// output pipes open. Pipe closure wakes the wait early, so this only bounds
/// how late a child that exits without closing them is noticed.
const POLL: Duration = Duration::from_millis(50);

/// A finite process observation. Limits apply independently to both streams.
#[derive(Debug)]
pub(crate) enum LimitedOutput {
    Completed(Output),
    Deadline,
    OutputLimit,
}

/// Bound memory and elapsed time even if a descendant holds a pipe open.
/// Only this invocation's direct child is killed; no process-name matching.
pub(crate) fn output_limited(
    command: &mut Command,
    budget: Duration,
    max_stream_bytes: usize,
) -> io::Result<LimitedOutput> {
    let started = Instant::now();
    let deadline = started
        .checked_add(budget)
        .ok_or_else(|| io::Error::other("process deadline overflow"))?;
    let cap = max_stream_bytes
        .checked_add(1)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| io::Error::other("process output limit overflow"))?;
    if Instant::now() >= deadline {
        return Ok(LimitedOutput::Deadline);
    }
    let mut child = LimitedChild(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing stdout"))?;
    let stderr = child
        .0
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing stderr"))?;
    let (sender, receiver) = mpsc::channel();
    for (is_stdout, stream) in [
        (true, Box::new(stdout) as Box<dyn Read + Send>),
        (false, Box::new(stderr) as Box<dyn Read + Send>),
    ] {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stream.take(cap).read_to_end(&mut bytes).map(|_| bytes);
            // The receiver can disappear when the invocation expires.
            crate::warn_unrecorded(
                "deliver bounded process output",
                sender.send((is_stdout, result)),
            );
        });
    }
    drop(sender);
    let mut stdout = None;
    let mut stderr = None;
    let mut status = None;
    loop {
        // A cached exit must never turn an expired observation into success.
        if Instant::now() >= deadline {
            return Ok(LimitedOutput::Deadline);
        }
        if status.is_none() {
            status = child.0.try_wait()?;
        }
        if let (Some(status), Some(stdout), Some(stderr)) =
            (status, stdout.as_mut(), stderr.as_mut())
        {
            if Instant::now() >= deadline {
                return Ok(LimitedOutput::Deadline);
            }
            return Ok(LimitedOutput::Completed(Output {
                status,
                stdout: std::mem::take(stdout),
                stderr: std::mem::take(stderr),
            }));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining.min(Duration::from_millis(10))) {
            Ok((is_stdout, result)) => {
                let bytes = result?;
                if bytes.len() > max_stream_bytes {
                    return Ok(LimitedOutput::OutputLimit);
                }
                if is_stdout {
                    stdout = Some(bytes);
                } else {
                    stderr = Some(bytes);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if stdout.is_none() || stderr.is_none() {
                    return Err(io::Error::other("process output reader disconnected"));
                }
                std::thread::sleep(remaining.min(Duration::from_millis(1)));
            }
        }
    }
}

/// Run a short-lived profile probe in a process group owned by this call.
///
/// Unlike [`output_limited`], this path also stops cooperative descendants
/// before releasing the unreaped leader that anchors the process-group ID.
/// Descendants that leave the group are outside this cleanup guarantee.
#[cfg(unix)]
pub(crate) fn output_limited_in_owned_group(
    command: &mut Command,
    budget: Duration,
    max_stream_bytes: usize,
) -> io::Result<LimitedOutput> {
    use std::os::unix::process::CommandExt;

    let started = Instant::now();
    let deadline = started
        .checked_add(budget)
        .ok_or_else(|| io::Error::other("process deadline overflow"))?;
    max_stream_bytes
        .checked_add(1)
        .ok_or_else(|| io::Error::other("process output limit overflow"))?;
    if Instant::now() >= deadline {
        return Ok(LimitedOutput::Deadline);
    }

    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let process_group = child.id() as i32;
    let mut child = OwnedProbeChild::new(child, process_group);
    let mut stdout_reader = child
        .child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing stdout"))?;
    let mut stderr_reader = child
        .child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing stderr"))?;
    set_probe_nonblocking(&stdout_reader)?;
    set_probe_nonblocking(&stderr_reader)?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    loop {
        if Instant::now() >= deadline {
            child.terminate_and_reap()?;
            return Ok(LimitedOutput::Deadline);
        }

        if !stdout_closed {
            stdout_closed = matches!(
                read_probe_stream(&mut stdout_reader, &mut stdout, max_stream_bytes)?,
                ProbeStreamRead::Closed
            );
            if stdout.len() > max_stream_bytes {
                child.terminate_and_reap()?;
                return Ok(LimitedOutput::OutputLimit);
            }
        }
        if !stderr_closed {
            stderr_closed = matches!(
                read_probe_stream(&mut stderr_reader, &mut stderr, max_stream_bytes)?,
                ProbeStreamRead::Closed
            );
            if stderr.len() > max_stream_bytes {
                child.terminate_and_reap()?;
                return Ok(LimitedOutput::OutputLimit);
            }
        }

        if child.leader_exited_without_reaping()? {
            // Even when the pipes are closed, a cooperative grandchild may
            // still be doing work. Stop the group while the zombie leader
            // still anchors its identity, then reap and verify group absence.
            let status = child.terminate_and_reap()?;
            let drain_deadline = Instant::now() + Duration::from_millis(250);
            while !stdout_closed || !stderr_closed {
                if !stdout_closed {
                    stdout_closed = matches!(
                        read_probe_stream(&mut stdout_reader, &mut stdout, max_stream_bytes)?,
                        ProbeStreamRead::Closed
                    );
                }
                if !stderr_closed {
                    stderr_closed = matches!(
                        read_probe_stream(&mut stderr_reader, &mut stderr, max_stream_bytes)?,
                        ProbeStreamRead::Closed
                    );
                }
                if stdout.len() > max_stream_bytes || stderr.len() > max_stream_bytes {
                    return Ok(LimitedOutput::OutputLimit);
                }
                if stdout_closed && stderr_closed {
                    break;
                }
                if Instant::now() >= drain_deadline {
                    return Err(io::Error::other(
                        "profile probe output remained open after owned-group cleanup",
                    ));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            return Ok(LimitedOutput::Completed(Output {
                status,
                stdout,
                stderr,
            }));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(unix))]
pub(crate) fn output_limited_in_owned_group(
    _command: &mut Command,
    _budget: Duration,
    _max_stream_bytes: usize,
) -> io::Result<LimitedOutput> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "owned process-group probes are unavailable on this platform",
    ))
}

#[cfg(unix)]
enum ProbeStreamRead {
    Open,
    Closed,
}

#[cfg(unix)]
fn set_probe_nonblocking<R: std::os::fd::AsRawFd>(stream: &R) -> io::Result<()> {
    let fd = stream.as_raw_fd();
    // SAFETY: F_GETFL only reads flags for the owned pipe descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: F_SETFL changes only the owned pipe's status flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn read_probe_stream<R: Read>(
    stream: &mut R,
    bytes: &mut Vec<u8>,
    max_stream_bytes: usize,
) -> io::Result<ProbeStreamRead> {
    let read_limit = max_stream_bytes
        .checked_add(1)
        .ok_or_else(|| io::Error::other("process output limit overflow"))?;
    let mut chunk = [0_u8; 4096];
    loop {
        let remaining = read_limit.saturating_sub(bytes.len());
        if remaining == 0 {
            return Ok(ProbeStreamRead::Open);
        }
        let read_len = chunk.len().min(remaining);
        match stream.read(&mut chunk[..read_len]) {
            Ok(0) => return Ok(ProbeStreamRead::Closed),
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(ProbeStreamRead::Open);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
struct OwnedProbeChild {
    child: Child,
    process_group: i32,
    reaped: bool,
    ownership_uncertain: bool,
}

#[cfg(unix)]
impl OwnedProbeChild {
    fn new(child: Child, process_group: i32) -> Self {
        Self {
            child,
            process_group,
            reaped: false,
            ownership_uncertain: false,
        }
    }

    fn leader_exited_without_reaping(&mut self) -> io::Result<bool> {
        // SAFETY: an all-zero siginfo_t is a valid output buffer for waitid.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: this invocation owns the unreaped child and uses WNOWAIT to
        // observe its exit without releasing the process-group identity.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            return Ok(info.si_signo != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        // ECHILD means another waiter released our process-group anchor.
        // Other waitid failures do not reap, so Drop can still clean up
        // while this Child remains the sole owner.
        self.ownership_uncertain = error.raw_os_error() == Some(libc::ECHILD);
        Err(error)
    }

    fn terminate_and_reap(&mut self) -> io::Result<ExitStatus> {
        if self.reaped {
            return Err(io::Error::other("owned profile probe was already reaped"));
        }

        let mut term_error = None;
        let mut kill_error = None;
        if !self.ownership_uncertain {
            if let Err(error) =
                crate::gates::signal_owned_process_group(self.process_group, libc::SIGTERM)
                && error.raw_os_error() != Some(libc::ESRCH)
            {
                term_error = Some(error);
            }
            // Keep the leader unreaped throughout this grace period, even if
            // it already exited. Its zombie still anchors the group ID while
            // cooperative descendants handle TERM.
            std::thread::sleep(Duration::from_millis(75));
            if let Err(error) =
                crate::gates::signal_owned_process_group(self.process_group, libc::SIGKILL)
                && error.raw_os_error() != Some(libc::ESRCH)
            {
                kill_error = Some(error);
            }
        }

        // Never block in `Child::wait` unless WNOWAIT has already observed
        // exit. A SIGKILLed process can remain stuck in uninterruptible kernel
        // work; in that case this bounded cleanup reports uncertainty instead
        // of waiting indefinitely or signaling again from Drop.
        let reap_deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match self.leader_exited_without_reaping() {
                Ok(true) => {
                    let status = match self.child.wait() {
                        Ok(status) => {
                            self.reaped = true;
                            status
                        }
                        Err(error) => {
                            self.ownership_uncertain = true;
                            return Err(error);
                        }
                    };
                    let final_group_absence =
                        Ok(crate::gates::gate_process_group_absent(self.process_group));
                    validate_owned_probe_cleanup(
                        self.reaped,
                        final_group_absence,
                        term_error,
                        kill_error,
                    )?;
                    return Ok(status);
                }
                Ok(false) if Instant::now() < reap_deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(false) => {
                    self.ownership_uncertain = true;
                    return Err(io::Error::other(
                        "owned profile probe leader did not exit before cleanup deadline",
                    ));
                }
                Err(error) => {
                    self.ownership_uncertain = true;
                    return Err(error);
                }
            }
        }
    }
}

#[cfg(unix)]
fn validate_owned_probe_cleanup(
    leader_reaped: bool,
    final_group_absence: io::Result<bool>,
    term_error: Option<io::Error>,
    kill_error: Option<io::Error>,
) -> io::Result<()> {
    if !leader_reaped {
        return Err(io::Error::other(
            "owned profile probe group absence was checked before reaping its leader",
        ));
    }

    let signal_errors = [("SIGTERM", term_error), ("SIGKILL", kill_error)]
        .into_iter()
        .filter_map(|(signal, error)| error.map(|error| format!("{signal}: {error}")))
        .collect::<Vec<_>>();
    let signal_note = if signal_errors.is_empty() {
        String::new()
    } else {
        format!(" (signal errors: {})", signal_errors.join(", "))
    };

    match final_group_absence {
        // A signal error is discharged only after the leader has been reaped
        // and the final group-absence probe succeeds.
        Ok(true) => Ok(()),
        Ok(false) => Err(io::Error::other(format!(
            "owned profile probe process group remained after reaping{signal_note}"
        ))),
        Err(error) => Err(io::Error::other(format!(
            "could not prove owned profile probe process group absent after reaping: {error}{signal_note}"
        ))),
    }
}

#[cfg(unix)]
impl Drop for OwnedProbeChild {
    fn drop(&mut self) {
        if !self.reaped && !self.ownership_uncertain {
            crate::warn_unrecorded(
                "cleanup owned profile probe group",
                self.terminate_and_reap().map(|_| ()),
            );
        }
    }
}

struct LimitedChild(std::process::Child);

impl Drop for LimitedChild {
    fn drop(&mut self) {
        match self.0.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {
                crate::warn_unrecorded("kill bounded child", self.0.kill());
                crate::warn_unrecorded("reap bounded child", self.0.wait());
            }
        }
    }
}

/// Run `command` like [`Command::output`], but kill it once `budget` elapses.
///
/// `Ok(None)` means the budget expired: the child was killed and reaped, and
/// whatever it printed is discarded. Stdin is null, as with
/// `Command::output`, so a child cannot block on a prompt nobody will answer.
///
/// The output readers run on their own threads, so a child that fills a pipe
/// cannot deadlock against the wait. On expiry the readers are detached, not
/// joined: a grandchild that inherited the pipes could otherwise hold the
/// caller past the deadline it was promised.
pub(crate) fn output_within(command: &mut Command, budget: Duration) -> io::Result<Option<Output>> {
    let deadline = Instant::now() + budget;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let (done_tx, done_rx) = mpsc::channel();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("child stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("child stderr was not piped"))?;
    let stdout_reader = spawn_reader(stdout, done_tx.clone());
    let stderr_reader = spawn_reader(stderr, done_tx);

    let mut open_streams = 2_u8;
    let mut exited = None;
    let status = loop {
        if exited.is_none() {
            match child.try_wait() {
                Ok(status) => exited = status,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
        }
        if let (Some(status), 0) = (exited, open_streams) {
            break status;
        }
        let now = Instant::now();
        if now >= deadline {
            if exited.is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Ok(None);
        }
        let remaining = deadline - now;
        if open_streams == 0 {
            // Both pipes closed; exit follows almost at once. The channel is
            // disconnected by now, so it cannot be used to sleep on.
            std::thread::sleep(remaining.min(Duration::from_millis(1)));
            continue;
        }
        match done_rx.recv_timeout(remaining.min(POLL)) {
            Ok(()) => open_streams -= 1,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => open_streams = 0,
        }
    };
    Ok(Some(Output {
        status,
        stdout: join_reader(stdout_reader)?,
        stderr: join_reader(stderr_reader)?,
    }))
}

fn spawn_reader<R>(mut stream: R, done: mpsc::Sender<()>) -> JoinHandle<io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stream.read_to_end(&mut bytes).map(|_| bytes);
        let _ = done.send(());
        result
    })
}

fn join_reader(reader: JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    reader
        .join()
        .map_err(|_| io::Error::other("child output reader panicked"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_output_retains_status_and_both_streams() {
        let LimitedOutput::Completed(output) = output_limited(
            Command::new("sh").args(["-c", "printf out; printf err >&2; exit 7"]),
            Duration::from_secs(5),
            3,
        )
        .unwrap() else {
            panic!("finite child must complete");
        };
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
    }

    #[test]
    fn limited_output_refuses_either_stream_overflow() {
        for script in ["printf four", "printf four >&2"] {
            assert!(matches!(
                output_limited(
                    Command::new("sh").args(["-c", script]),
                    Duration::from_secs(5),
                    3,
                )
                .unwrap(),
                LimitedOutput::OutputLimit
            ));
        }
    }

    #[test]
    fn limited_output_refuses_zero_budget_without_launching() {
        assert!(matches!(
            output_limited(
                &mut Command::new("aethyme-must-not-exist"),
                Duration::ZERO,
                32,
            )
            .unwrap(),
            LimitedOutput::Deadline
        ));
    }

    #[test]
    fn limited_output_kills_only_its_own_expired_child() {
        let started = Instant::now();
        assert!(matches!(
            output_limited(
                Command::new("sh").args(["-c", "exec sleep 30"]),
                Duration::from_millis(100),
                32,
            )
            .unwrap(),
            LimitedOutput::Deadline
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn owned_probe_cleanup_discharges_kill_eperm_only_after_final_absence() {
        // TERM succeeded; KILL can report EPERM when the only remaining group
        // member is the unreaped zombie leader. That error is safe to discharge
        // only after reaping and a successful final absence probe.
        assert!(
            validate_owned_probe_cleanup(
                true,
                Ok(true),
                None,
                Some(io::Error::from_raw_os_error(libc::EPERM)),
            )
            .is_ok()
        );

        assert!(
            validate_owned_probe_cleanup(
                true,
                Ok(false),
                None,
                Some(io::Error::from_raw_os_error(libc::EPERM)),
            )
            .is_err(),
            "a surviving member must remain an error"
        );
        assert!(
            validate_owned_probe_cleanup(
                true,
                Err(io::Error::from_raw_os_error(libc::EPERM)),
                None,
                Some(io::Error::from_raw_os_error(libc::EPERM)),
            )
            .is_err(),
            "an unknown final probe must remain an error"
        );
        assert!(
            validate_owned_probe_cleanup(
                false,
                Ok(true),
                None,
                Some(io::Error::from_raw_os_error(libc::EPERM)),
            )
            .is_err(),
            "absence cannot discharge an error before the leader is reaped"
        );
    }

    #[cfg(unix)]
    #[test]
    fn owned_probe_group_accepts_signal_error_after_successful_leader_exit() {
        let started = Instant::now();
        let output = output_limited_in_owned_group(
            Command::new("sh").args(["-c", "exit 0"]),
            Duration::from_secs(2),
            64,
        )
        .expect("a final process-group absence proof should discharge a zombie-only signal error");
        assert!(started.elapsed() < Duration::from_secs(2));
        match output {
            LimitedOutput::Completed(output) => {
                assert_eq!(output.status.code(), Some(0));
                assert!(output.stdout.is_empty());
                assert!(output.stderr.is_empty());
            }
            _ => panic!("an immediately exiting leader should return its output"),
        }
    }

    #[cfg(unix)]
    fn assert_owned_probe_stops_cooperative_descendant(
        script: &str,
        budget: Duration,
        max_stream_bytes: usize,
        expected: fn(LimitedOutput),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("descendant-survived");
        let started = Instant::now();
        let output = output_limited_in_owned_group(
            Command::new("sh")
                .args(["-c", script])
                .env("PROFILE_PROBE_MARKER", &marker),
            budget,
            max_stream_bytes,
        )
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "owned-group cleanup must stay within its finite bound"
        );
        expected(output);
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            !marker.exists(),
            "cooperative descendant must be stopped before the owned probe returns"
        );
    }

    #[cfg(unix)]
    #[test]
    fn owned_probe_group_deadline_stops_cooperative_descendant() {
        assert_owned_probe_stops_cooperative_descendant(
            "(sleep 0.45; printf escaped > \"$PROFILE_PROBE_MARKER\") & wait",
            Duration::from_millis(80),
            64,
            |output| assert!(matches!(output, LimitedOutput::Deadline)),
        );
    }

    #[cfg(unix)]
    #[test]
    fn owned_probe_group_output_limit_stops_cooperative_descendant() {
        assert_owned_probe_stops_cooperative_descendant(
            "(sleep 0.45; printf escaped > \"$PROFILE_PROBE_MARKER\") & printf overflow; wait",
            Duration::from_secs(2),
            3,
            |output| assert!(matches!(output, LimitedOutput::OutputLimit)),
        );
    }

    #[cfg(unix)]
    #[test]
    fn owned_probe_group_cleans_closed_pipe_descendant_after_leader_exit() {
        assert_owned_probe_stops_cooperative_descendant(
            "(sleep 0.45; printf escaped > \"$PROFILE_PROBE_MARKER\") >/dev/null 2>&1 & exit 0",
            Duration::from_secs(2),
            64,
            |output| match output {
                LimitedOutput::Completed(output) => {
                    assert_eq!(output.status.code(), Some(0));
                    assert!(output.stdout.is_empty());
                    assert!(output.stderr.is_empty());
                }
                _ => panic!("a finite leader exit should return its output"),
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn owned_probe_group_cleans_pipe_holding_descendant_after_leader_exit() {
        assert_owned_probe_stops_cooperative_descendant(
            "(sleep 0.45; printf escaped > \"$PROFILE_PROBE_MARKER\") & printf leader; exit 0",
            Duration::from_secs(2),
            64,
            |output| match output {
                LimitedOutput::Completed(output) => {
                    assert_eq!(output.status.code(), Some(0));
                    assert_eq!(output.stdout, b"leader");
                    assert!(output.stderr.is_empty());
                }
                _ => panic!("a finite leader exit should return its output"),
            },
        );
    }

    #[test]
    fn a_child_that_finishes_in_time_returns_its_output_and_status() {
        let output = output_within(
            Command::new("sh").args(["-c", "printf out; printf err >&2; exit 3"]),
            Duration::from_secs(30),
        )
        .unwrap()
        .expect("finished within the budget");
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
    }

    #[test]
    fn a_child_that_outlives_the_budget_is_killed_and_reported() {
        let started = Instant::now();
        let output = output_within(
            Command::new("sh").args(["-c", "exec sleep 30"]),
            Duration::from_millis(200),
        )
        .unwrap();
        assert!(output.is_none(), "a sleeping child must time out");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the wait must end near the budget, not when the child does"
        );
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_does_not_deadlock() {
        let output = output_within(
            Command::new("sh").args(["-c", "head -c 1000000 /dev/zero"]),
            Duration::from_secs(30),
        )
        .unwrap()
        .expect("finished within the budget");
        assert_eq!(output.stdout.len(), 1_000_000);
    }
}
