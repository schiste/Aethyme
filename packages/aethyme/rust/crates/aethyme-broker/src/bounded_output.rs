//! Run a child process to completion within a wall-clock budget.
//!
//! `Command::output` has no deadline, so a wedged child (a git waiting on an
//! unreachable remote, a lock held by a dead process) hangs its caller for as
//! long as the child lives (#219). This is the generic bounded variant. The
//! admission lane in `operations.rs` has its own `output_within`, which is
//! bound to that lane's error type and heartbeat; it can move onto this helper
//! in a follow-up.

use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
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
            if sender.send((is_stdout, result)).is_err() {
                return;
            }
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
        if status.is_some() && stdout.is_some() && stderr.is_some() {
            if Instant::now() >= deadline {
                return Ok(LimitedOutput::Deadline);
            }
            return Ok(LimitedOutput::Completed(Output {
                status: status.expect("checked status"),
                stdout: stdout.expect("checked stdout"),
                stderr: stderr.expect("checked stderr"),
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
