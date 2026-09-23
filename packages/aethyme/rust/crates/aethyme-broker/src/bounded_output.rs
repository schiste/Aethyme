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
