//! Run a child process to completion within a wall-clock budget.
//!
//! `Command::output` has no deadline, so a wedged child (a git waiting on an
//! unreachable remote, a lock held by a dead process) hangs its caller for as
//! long as the child lives (#219). This is the generic bounded variant. The
//! admission lane in `operations.rs` has its own `output_within`, which is
//! bound to that lane's error type and heartbeat; it can move onto this helper
//! in a follow-up.

use std::fs::File;
use std::io::{self, Read, Write};
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

/// Stream a child's stdout to a caller-owned sink without retaining the
/// payload in memory. The byte probe past each cap is never written.
#[derive(Debug)]
pub(crate) enum LimitedStreamOutput {
    Completed {
        status: ExitStatus,
        stdout_bytes: u64,
        stderr: Vec<u8>,
    },
    Deadline,
    StdoutLimit,
    StderrLimit,
}

/// Bound memory and elapsed time even if a descendant holds a pipe open.
/// Only this invocation's direct child is killed; no process-name matching.
pub(crate) fn output_limited(
    command: &mut Command,
    budget: Duration,
    max_stream_bytes: usize,
) -> io::Result<LimitedOutput> {
    #[cfg(any(unix, windows))]
    {
        capture_with_stdin(command, Stdio::null(), budget, max_stream_bytes)
    }
    #[cfg(not(any(unix, windows)))]
    {
        output_limited_fallback(command, budget, max_stream_bytes)
    }
}

// Preserve the existing implementation on targets without a nonblocking pipe
// probe. Supported Unix/Windows paths own their pipe handles in the caller and
// leave no detached output readers after cancellation.
#[cfg(not(any(unix, windows)))]
fn output_limited_fallback(
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

/// Run one child with a finite file-backed stdin and bounded captured output.
///
/// Like the null-input capture on Unix/Windows, this polls owned pipe handles
/// directly. A descendant holding a pipe cannot leave a detached output reader
/// blocked after the deadline.
pub(crate) fn output_with_input_limited(
    command: &mut Command,
    input: File,
    budget: Duration,
    max_stream_bytes: usize,
) -> io::Result<LimitedOutput> {
    capture_with_stdin(command, Stdio::from(input), budget, max_stream_bytes)
}

fn capture_with_stdin(
    command: &mut Command,
    input: Stdio,
    budget: Duration,
    max_stream_bytes: usize,
) -> io::Result<LimitedOutput> {
    let deadline = checked_deadline(budget)?;
    checked_stream_limit(max_stream_bytes)?;
    if Instant::now() >= deadline {
        return Ok(LimitedOutput::Deadline);
    }

    let mut child = LimitedChild(
        command
            .stdin(input)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let mut stdout_reader = child
        .0
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing stdout"))?;
    let mut stderr_reader = child
        .0
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing stderr"))?;
    set_probe_nonblocking(&stdout_reader)?;
    set_probe_nonblocking(&stderr_reader)?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    let mut status = None;
    loop {
        if Instant::now() >= deadline {
            return Ok(LimitedOutput::Deadline);
        }

        if !stdout_closed {
            stdout_closed = matches!(
                read_probe_stream(&mut stdout_reader, &mut stdout, max_stream_bytes, deadline)?,
                ProbeStreamRead::Closed
            );
            if stdout.len() > max_stream_bytes {
                return Ok(LimitedOutput::OutputLimit);
            }
        }
        if !stderr_closed {
            stderr_closed = matches!(
                read_probe_stream(&mut stderr_reader, &mut stderr, max_stream_bytes, deadline)?,
                ProbeStreamRead::Closed
            );
            if stderr.len() > max_stream_bytes {
                return Ok(LimitedOutput::OutputLimit);
            }
        }
        if status.is_none() {
            status = child.0.try_wait()?;
        }
        if stdout_closed
            && stderr_closed
            && let Some(status) = status.as_ref()
        {
            if Instant::now() >= deadline {
                return Ok(LimitedOutput::Deadline);
            }
            return Ok(LimitedOutput::Completed(Output {
                status: *status,
                stdout,
                stderr,
            }));
        }
        sleep_until_poll(deadline);
    }
}

/// Run two children as a streaming pipeline with finite stdin for the first
/// child and bounded output capture from both children. The first child's
/// stdout remains a pipe into the second; patch data is never buffered here.
pub(crate) fn pipeline_with_input_limited(
    first: &mut Command,
    input: File,
    second: &mut Command,
    budget: Duration,
    max_stream_bytes: usize,
) -> io::Result<LimitedPipelineOutput> {
    let deadline = checked_deadline(budget)?;
    checked_stream_limit(max_stream_bytes)?;
    if Instant::now() >= deadline {
        return Ok(LimitedPipelineOutput::Deadline);
    }

    let mut first = LimitedChild(
        first
            .stdin(Stdio::from(input))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let first_stdout = first
        .0
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing first-child stdout"))?;
    let mut first_stderr = first
        .0
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing first-child stderr"))?;
    let (mut first, mut second) = spawn_pipeline_second(first, first_stdout, second)?;
    let mut second_stdout = second
        .0
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing second-child stdout"))?;
    let mut second_stderr = second
        .0
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing second-child stderr"))?;
    set_probe_nonblocking(&first_stderr)?;
    set_probe_nonblocking(&second_stdout)?;
    set_probe_nonblocking(&second_stderr)?;

    let mut first_stderr_bytes = Vec::new();
    let mut second_stdout_bytes = Vec::new();
    let mut second_stderr_bytes = Vec::new();
    let mut first_stderr_closed = false;
    let mut second_stdout_closed = false;
    let mut second_stderr_closed = false;
    let mut first_status = None;
    let mut second_status = None;
    loop {
        if Instant::now() >= deadline {
            return Ok(LimitedPipelineOutput::Deadline);
        }

        if !first_stderr_closed {
            first_stderr_closed = matches!(
                read_probe_stream(
                    &mut first_stderr,
                    &mut first_stderr_bytes,
                    max_stream_bytes,
                    deadline,
                )?,
                ProbeStreamRead::Closed
            );
            if first_stderr_bytes.len() > max_stream_bytes {
                return Ok(LimitedPipelineOutput::OutputLimit);
            }
        }
        if !second_stdout_closed {
            second_stdout_closed = matches!(
                read_probe_stream(
                    &mut second_stdout,
                    &mut second_stdout_bytes,
                    max_stream_bytes,
                    deadline
                )?,
                ProbeStreamRead::Closed
            );
            if second_stdout_bytes.len() > max_stream_bytes {
                return Ok(LimitedPipelineOutput::OutputLimit);
            }
        }
        if !second_stderr_closed {
            second_stderr_closed = matches!(
                read_probe_stream(
                    &mut second_stderr,
                    &mut second_stderr_bytes,
                    max_stream_bytes,
                    deadline
                )?,
                ProbeStreamRead::Closed
            );
            if second_stderr_bytes.len() > max_stream_bytes {
                return Ok(LimitedPipelineOutput::OutputLimit);
            }
        }
        if first_status.is_none() {
            first_status = first.0.try_wait()?;
        }
        if second_status.is_none() {
            second_status = second.0.try_wait()?;
        }
        if first_stderr_closed
            && second_stdout_closed
            && second_stderr_closed
            && let (Some(first_status), Some(second_status)) =
                (first_status.as_ref(), second_status.as_ref())
        {
            if Instant::now() >= deadline {
                return Ok(LimitedPipelineOutput::Deadline);
            }
            return Ok(LimitedPipelineOutput::Completed {
                first_status: *first_status,
                first_stderr: first_stderr_bytes,
                second: Output {
                    status: *second_status,
                    stdout: second_stdout_bytes,
                    stderr: second_stderr_bytes,
                },
            });
        }
        sleep_until_poll(deadline);
    }
}

fn spawn_pipeline_second(
    first: LimitedChild,
    first_stdout: std::process::ChildStdout,
    second: &mut Command,
) -> io::Result<(LimitedChild, LimitedChild)> {
    let second = LimitedChild(
        second
            .stdin(Stdio::from(first_stdout))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    Ok((first, second))
}

#[derive(Debug)]
pub(crate) enum LimitedPipelineOutput {
    Completed {
        first_status: ExitStatus,
        first_stderr: Vec<u8>,
        second: Output,
    },
    Deadline,
    OutputLimit,
}

fn checked_deadline(budget: Duration) -> io::Result<Instant> {
    Instant::now()
        .checked_add(budget)
        .ok_or_else(|| io::Error::other("process deadline overflow"))
}

fn checked_stream_limit(max_stream_bytes: usize) -> io::Result<()> {
    max_stream_bytes
        .checked_add(1)
        .ok_or_else(|| io::Error::other("process output limit overflow"))?;
    Ok(())
}

fn sleep_until_poll(deadline: Instant) {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if !remaining.is_zero() {
        std::thread::sleep(remaining.min(Duration::from_millis(2)));
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
                read_probe_stream(&mut stdout_reader, &mut stdout, max_stream_bytes, deadline)?,
                ProbeStreamRead::Closed
            );
            if stdout.len() > max_stream_bytes {
                child.terminate_and_reap()?;
                return Ok(LimitedOutput::OutputLimit);
            }
        }
        if !stderr_closed {
            stderr_closed = matches!(
                read_probe_stream(&mut stderr_reader, &mut stderr, max_stream_bytes, deadline)?,
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
                        read_probe_stream(
                            &mut stdout_reader,
                            &mut stdout,
                            max_stream_bytes,
                            drain_deadline,
                        )?,
                        ProbeStreamRead::Closed
                    );
                }
                if !stderr_closed {
                    stderr_closed = matches!(
                        read_probe_stream(
                            &mut stderr_reader,
                            &mut stderr,
                            max_stream_bytes,
                            drain_deadline,
                        )?,
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

/// Stream stdout into `writer` while independently bounding stderr, the
/// process-group lifetime and the bytes written to the sink. Deadline checks
/// surround pipe reads and sink writes; a sink blocked in the operating system
/// can still outlive the deadline while that write is in progress.
#[cfg(unix)]
pub(crate) fn stream_stdout_to_writer_limited_in_owned_group<W: Write>(
    command: &mut Command,
    budget: Duration,
    max_stdout_bytes: u64,
    max_stderr_bytes: usize,
    writer: &mut W,
) -> io::Result<LimitedStreamOutput> {
    use std::os::unix::process::CommandExt;

    let started = Instant::now();
    let deadline = started
        .checked_add(budget)
        .ok_or_else(|| io::Error::other("process deadline overflow"))?;
    max_stdout_bytes
        .checked_add(1)
        .ok_or_else(|| io::Error::other("process stdout limit overflow"))?;
    max_stderr_bytes
        .checked_add(1)
        .ok_or_else(|| io::Error::other("process stderr limit overflow"))?;
    if Instant::now() >= deadline {
        return Ok(LimitedStreamOutput::Deadline);
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

    let mut stdout_bytes = 0_u64;
    let mut stderr = Vec::new();
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    loop {
        if Instant::now() >= deadline {
            child.terminate_and_reap()?;
            return Ok(LimitedStreamOutput::Deadline);
        }

        if !stdout_closed {
            match write_probe_stream(
                &mut stdout_reader,
                writer,
                &mut stdout_bytes,
                max_stdout_bytes,
                deadline,
            )? {
                WriteProbeStreamRead::Open => {}
                WriteProbeStreamRead::Closed => stdout_closed = true,
                WriteProbeStreamRead::OutputLimit => {
                    child.terminate_and_reap()?;
                    return Ok(LimitedStreamOutput::StdoutLimit);
                }
            }
        }
        if !stderr_closed {
            stderr_closed = matches!(
                read_probe_stream(&mut stderr_reader, &mut stderr, max_stderr_bytes, deadline)?,
                ProbeStreamRead::Closed
            );
            if stderr.len() > max_stderr_bytes {
                child.terminate_and_reap()?;
                return Ok(LimitedStreamOutput::StderrLimit);
            }
        }

        if child.leader_exited_without_reaping()? {
            // Keep the leader unreaped until the owned group is stopped. This
            // prevents its PID/group identity from being reused before cleanup.
            let status = child.terminate_and_reap()?;
            let drain_deadline = Instant::now()
                .checked_add(Duration::from_millis(250))
                .ok_or_else(|| io::Error::other("pipe drain deadline overflow"))?;
            while !stdout_closed || !stderr_closed {
                if !stdout_closed {
                    match write_probe_stream(
                        &mut stdout_reader,
                        writer,
                        &mut stdout_bytes,
                        max_stdout_bytes,
                        drain_deadline,
                    )? {
                        WriteProbeStreamRead::Open => {}
                        WriteProbeStreamRead::Closed => stdout_closed = true,
                        WriteProbeStreamRead::OutputLimit => {
                            return Ok(LimitedStreamOutput::StdoutLimit);
                        }
                    }
                }
                if !stderr_closed {
                    stderr_closed = matches!(
                        read_probe_stream(
                            &mut stderr_reader,
                            &mut stderr,
                            max_stderr_bytes,
                            drain_deadline,
                        )?,
                        ProbeStreamRead::Closed
                    );
                }
                if stderr.len() > max_stderr_bytes {
                    return Ok(LimitedStreamOutput::StderrLimit);
                }
                if stdout_closed && stderr_closed {
                    if Instant::now() >= deadline {
                        return Ok(LimitedStreamOutput::Deadline);
                    }
                    return Ok(LimitedStreamOutput::Completed {
                        status,
                        stdout_bytes,
                        stderr,
                    });
                }
                if Instant::now() >= drain_deadline {
                    return Err(io::Error::other(
                        "child output remained open after owned-group cleanup",
                    ));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if Instant::now() >= deadline {
                return Ok(LimitedStreamOutput::Deadline);
            }
            return Ok(LimitedStreamOutput::Completed {
                status,
                stdout_bytes,
                stderr,
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(unix))]
pub(crate) fn stream_stdout_to_writer_limited_in_owned_group<W: Write>(
    _command: &mut Command,
    _budget: Duration,
    _max_stdout_bytes: u64,
    _max_stderr_bytes: usize,
    _writer: &mut W,
) -> io::Result<LimitedStreamOutput> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "owned process-group streaming is unavailable on this platform",
    ))
}

#[cfg(unix)]
enum WriteProbeStreamRead {
    Open,
    Closed,
    OutputLimit,
}

#[cfg(unix)]
fn write_probe_stream<R: Read, W: Write>(
    stream: &mut R,
    writer: &mut W,
    written: &mut u64,
    max_bytes: u64,
    deadline: Instant,
) -> io::Result<WriteProbeStreamRead> {
    let mut chunk = [0_u8; 4096];
    loop {
        if Instant::now() >= deadline {
            return Ok(WriteProbeStreamRead::Open);
        }
        let remaining = max_bytes.saturating_sub(*written);
        let read_len = usize::try_from(remaining.saturating_add(1).min(chunk.len() as u64))
            .map_err(|_| io::Error::other("process stdout read bound overflow"))?;
        match stream.read(&mut chunk[..read_len]) {
            Ok(0) => return Ok(WriteProbeStreamRead::Closed),
            Ok(count) => {
                let allowed = usize::try_from(remaining.min(count as u64))
                    .map_err(|_| io::Error::other("process stdout byte count overflow"))?;
                if allowed > 0 {
                    writer.write_all(&chunk[..allowed])?;
                    *written = written
                        .checked_add(allowed as u64)
                        .ok_or_else(|| io::Error::other("process stdout byte count overflow"))?;
                }
                if count > allowed {
                    return Ok(WriteProbeStreamRead::OutputLimit);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(WriteProbeStreamRead::Open);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
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

#[cfg(any(unix, windows))]
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
    deadline: Instant,
) -> io::Result<ProbeStreamRead> {
    let read_limit = max_stream_bytes
        .checked_add(1)
        .ok_or_else(|| io::Error::other("process output limit overflow"))?;
    let mut chunk = [0_u8; 4096];
    loop {
        if Instant::now() >= deadline {
            return Ok(ProbeStreamRead::Open);
        }
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

#[cfg(windows)]
fn set_probe_nonblocking<R>(_stream: &R) -> io::Result<()> {
    // Windows anonymous pipes are polled with PeekNamedPipe before each read,
    // so the subsequent synchronous read is limited to bytes already ready.
    Ok(())
}

#[cfg(windows)]
fn read_probe_stream<R: Read + std::os::windows::io::AsRawHandle>(
    stream: &mut R,
    bytes: &mut Vec<u8>,
    max_stream_bytes: usize,
    deadline: Instant,
) -> io::Result<ProbeStreamRead> {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn PeekNamedPipe(
            pipe: *mut c_void,
            buffer: *mut c_void,
            buffer_size: u32,
            bytes_read: *mut u32,
            total_bytes_available: *mut u32,
            bytes_left_this_message: *mut u32,
        ) -> i32;
    }

    let read_limit = max_stream_bytes
        .checked_add(1)
        .ok_or_else(|| io::Error::other("process output limit overflow"))?;
    let mut chunk = [0_u8; 4096];
    loop {
        if Instant::now() >= deadline {
            return Ok(ProbeStreamRead::Open);
        }
        let remaining = read_limit.saturating_sub(bytes.len());
        if remaining == 0 {
            return Ok(ProbeStreamRead::Open);
        }
        let mut available = 0_u32;
        // SAFETY: the handle is an owned child pipe, and the zero-length
        // buffer asks only for the number of bytes currently available.
        let peeked = unsafe {
            PeekNamedPipe(
                stream.as_raw_handle().cast(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if peeked == 0 {
            let error = io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(109 | 232)) {
                return Ok(ProbeStreamRead::Closed);
            }
            return Err(error);
        }
        if available == 0 {
            return Ok(ProbeStreamRead::Open);
        }
        let read_len = chunk
            .len()
            .min(remaining)
            .min(usize::try_from(available).unwrap_or(usize::MAX));
        match stream.read(&mut chunk[..read_len]) {
            Ok(0) => return Ok(ProbeStreamRead::Closed),
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(not(any(unix, windows)))]
enum ProbeStreamRead {
    Open,
    Closed,
}

#[cfg(not(any(unix, windows)))]
fn set_probe_nonblocking<R>(_stream: &R) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "bounded child pipes are unavailable on this platform",
    ))
}

#[cfg(not(any(unix, windows)))]
fn read_probe_stream<R: Read>(
    _stream: &mut R,
    _bytes: &mut Vec<u8>,
    _max_stream_bytes: usize,
    _deadline: Instant,
) -> io::Result<ProbeStreamRead> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "bounded child pipes are unavailable on this platform",
    ))
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
    use std::io::{Seek, SeekFrom, Write};

    #[cfg(unix)]
    fn assert_process_absent(pid: u32) {
        // SAFETY: signal zero only checks whether the child PID still exists.
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        assert_eq!(result, -1, "child process {pid} must have been reaped");
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "child process {pid} must be absent, not merely inaccessible"
        );
    }

    fn file_input(bytes: &[u8]) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file
    }

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
    fn expired_output_closes_pipes_still_inherited_by_a_descendant() {
        let directory = tempfile::tempdir().unwrap();
        let result_path = directory.path().join("pipe-result");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "(trap '' PIPE; sleep 0.2; if printf x 2>/dev/null; then printf open > \"$PIPE_RESULT\"; else printf closed > \"$PIPE_RESULT\"; fi) & wait",
        ]);
        command.env("PIPE_RESULT", &result_path);
        assert!(matches!(
            output_limited(&mut command, Duration::from_millis(80), 32).unwrap(),
            LimitedOutput::Deadline
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match std::fs::read(&result_path) {
                Ok(bytes) if !bytes.is_empty() => {
                    assert_eq!(bytes, b"closed", "cancelled capture must close its readers");
                    break;
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("read descendant result: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "descendant did not report pipe state"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn bounded_file_input_returns_output_and_status() {
        let expected = vec![b'x'; 128 * 1024];
        let LimitedOutput::Completed(output) = output_with_input_limited(
            &mut Command::new("cat"),
            file_input(&expected),
            Duration::from_secs(5),
            expected.len(),
        )
        .unwrap() else {
            panic!("finite file-backed input must complete");
        };
        assert!(output.status.success());
        assert_eq!(output.stdout, expected);
        assert!(output.stderr.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn bounded_file_input_refuses_zero_budget_without_spawning() {
        assert!(matches!(
            output_with_input_limited(
                &mut Command::new("bounded-input-must-not-spawn"),
                file_input(b""),
                Duration::ZERO,
                64,
            )
            .unwrap(),
            LimitedOutput::Deadline
        ));
    }

    #[cfg(unix)]
    #[test]
    fn bounded_file_input_kills_its_direct_child_at_deadline() {
        let started = Instant::now();
        assert!(matches!(
            output_with_input_limited(
                Command::new("sh").args(["-c", "exec sleep 30"]),
                file_input(b"HEAD:src/slow-read.rs\n"),
                Duration::from_millis(80),
                64,
            )
            .unwrap(),
            LimitedOutput::Deadline
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn bounded_file_input_returns_on_deadline_when_a_descendant_holds_a_pipe() {
        let started = Instant::now();
        assert!(matches!(
            output_with_input_limited(
                Command::new("sh").args(["-c", "(sleep 1.5) & exit 0"]),
                file_input(b""),
                Duration::from_millis(80),
                64,
            )
            .unwrap(),
            LimitedOutput::Deadline
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(1600));
    }

    #[cfg(unix)]
    #[test]
    fn bounded_pipeline_streams_file_input_through_large_intermediate_output() {
        let expected = vec![b'x'; 128 * 1024];
        let mut first = Command::new("cat");
        let mut second = Command::new("cat");
        let LimitedPipelineOutput::Completed {
            first_status,
            first_stderr,
            second,
        } = pipeline_with_input_limited(
            &mut first,
            file_input(&expected),
            &mut second,
            Duration::from_secs(5),
            expected.len(),
        )
        .unwrap()
        else {
            panic!("finite pipeline must complete");
        };
        assert!(first_status.success());
        assert!(second.status.success());
        assert!(first_stderr.is_empty());
        assert!(second.stderr.is_empty());
        assert_eq!(second.stdout, expected);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_pipeline_output_limit_reaps_both_owned_children() {
        let dir = tempfile::tempdir().unwrap();
        let first_pid_path = dir.path().join("first-pid");
        let second_pid_path = dir.path().join("second-pid");
        let mut first = Command::new("sh");
        first.args([
            "-c",
            "echo $$ > \"$1\"; printf 'go\\n'; exec sleep 30",
            "sh",
        ]);
        first.arg(&first_pid_path);
        let mut second = Command::new("sh");
        second.args([
            "-c",
            "IFS= read -r _; echo $$ > \"$1\"; printf overflow; exec sleep 30",
            "sh",
        ]);
        second.arg(&second_pid_path);
        assert!(matches!(
            pipeline_with_input_limited(
                &mut first,
                file_input(b""),
                &mut second,
                Duration::from_secs(5),
                3,
            )
            .unwrap(),
            LimitedPipelineOutput::OutputLimit
        ));
        let first_pid = std::fs::read_to_string(first_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let second_pid = std::fs::read_to_string(second_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_process_absent(first_pid);
        assert_process_absent(second_pid);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_pipeline_deadline_does_not_wait_for_an_inherited_pipe() {
        let started = Instant::now();
        let mut first = Command::new("sh");
        first.args(["-c", "(sleep 1.5) & exit 0"]);
        let mut second = Command::new("cat");
        assert!(matches!(
            pipeline_with_input_limited(
                &mut first,
                file_input(b""),
                &mut second,
                Duration::from_millis(80),
                64,
            )
            .unwrap(),
            LimitedPipelineOutput::Deadline
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(1600));
    }

    #[cfg(unix)]
    #[test]
    fn bounded_pipeline_reaps_first_child_when_second_spawn_fails() {
        let first = Command::new("sh")
            .args(["-c", "exec sleep 30"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let first_pid = first.id();
        let mut first = LimitedChild(first);
        let first_stdout = first.0.stdout.take().unwrap();
        let missing = tempfile::tempdir()
            .unwrap()
            .path()
            .join("missing-second-command");
        let mut second = Command::new(missing);
        assert!(spawn_pipeline_second(first, first_stdout, &mut second).is_err());
        assert_process_absent(first_pid);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_pipeline_reports_first_spawn_failure_without_starting_second() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("second-started");
        let mut first = Command::new(dir.path().join("missing-first-command"));
        let mut second = Command::new("sh");
        second.args(["-c", "touch \"$1\"", "sh"]);
        second.arg(&marker);
        let error = pipeline_with_input_limited(
            &mut first,
            file_input(b""),
            &mut second,
            Duration::from_secs(5),
            64,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!marker.exists(), "the second child must not start");
    }

    #[cfg(windows)]
    #[test]
    fn windows_pipeline_streams_output_and_observes_eof() {
        let mut first = Command::new("cmd");
        first.args(["/D", "/C", "echo bounded-pipeline"]);
        let mut second = Command::new("cmd");
        second.args(["/D", "/C", "more"]);
        let LimitedPipelineOutput::Completed {
            first_status,
            first_stderr,
            second,
        } = pipeline_with_input_limited(
            &mut first,
            file_input(b""),
            &mut second,
            Duration::from_secs(5),
            128,
        )
        .unwrap()
        else {
            panic!("finite Windows pipeline must close its captured pipes");
        };
        assert!(first_status.success());
        assert!(second.status.success());
        assert!(first_stderr.is_empty());
        assert!(second.stderr.is_empty());
        assert!(String::from_utf8_lossy(&second.stdout).contains("bounded-pipeline"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_pipeline_deadline_bounds_a_blocked_reader() {
        let started = Instant::now();
        let mut first = Command::new("cmd");
        first.args(["/D", "/C", "for /L %i in (1,1,2147483647) do @rem"]);
        let mut second = Command::new("cmd");
        second.args(["/D", "/C", "more"]);
        assert!(matches!(
            pipeline_with_input_limited(
                &mut first,
                file_input(b""),
                &mut second,
                Duration::from_millis(100),
                128,
            )
            .unwrap(),
            LimitedPipelineOutput::Deadline
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(windows)]
    #[test]
    fn windows_pipeline_enforces_captured_output_limit() {
        let mut first = Command::new("cmd");
        first.args(["/D", "/C", "echo overflow"]);
        let mut second = Command::new("cmd");
        second.args(["/D", "/C", "more"]);
        assert!(matches!(
            pipeline_with_input_limited(
                &mut first,
                file_input(b""),
                &mut second,
                Duration::from_secs(5),
                3,
            )
            .unwrap(),
            LimitedPipelineOutput::OutputLimit
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_pipe_polling_collects_output_and_observes_eof() {
        let LimitedOutput::Completed(output) = output_with_input_limited(
            Command::new("cmd").args(["/C", "echo bounded-pipe"]),
            file_input(b""),
            Duration::from_secs(5),
            128,
        )
        .unwrap() else {
            panic!("finite Windows child must close its captured pipes");
        };
        assert!(output.status.success());
        assert!(output.stdout.starts_with(b"bounded-pipe"));
        assert!(output.stderr.is_empty());
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

    #[cfg(unix)]
    #[test]
    fn owned_group_stream_writes_exact_stdout_cap_and_collects_bounded_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("streamed");
        let mut output_file = File::create(&output_path).unwrap();
        let output = stream_stdout_to_writer_limited_in_owned_group(
            Command::new("sh").args(["-c", "printf 12345678; printf note >&2"]),
            Duration::from_secs(2),
            8,
            16,
            &mut output_file,
        )
        .unwrap();

        match output {
            LimitedStreamOutput::Completed {
                status,
                stdout_bytes,
                stderr,
            } => {
                assert!(status.success());
                assert_eq!(stdout_bytes, 8);
                assert_eq!(stderr, b"note");
            }
            other => panic!("exact-cap stream should complete, got {other:?}"),
        }
        drop(output_file);
        assert_eq!(std::fs::read(output_path).unwrap(), b"12345678");
    }

    #[cfg(unix)]
    #[test]
    fn owned_group_stream_does_not_write_the_stdout_overflow_probe() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("streamed");
        let marker = dir.path().join("descendant-survived");
        let mut output_file = File::create(&output_path).unwrap();
        let output = stream_stdout_to_writer_limited_in_owned_group(
            Command::new("sh")
                .args([
                    "-c",
                    "(sleep 0.45; printf escaped > \"$PROFILE_PROBE_MARKER\") & printf 123456789; wait",
                ])
                .env("PROFILE_PROBE_MARKER", &marker),
            Duration::from_secs(2),
            8,
            16,
            &mut output_file,
        )
        .unwrap();
        assert!(matches!(output, LimitedStreamOutput::StdoutLimit));
        drop(output_file);
        assert_eq!(std::fs::read(output_path).unwrap(), b"12345678");
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            !marker.exists(),
            "overflow must stop the owned process group"
        );
    }

    #[cfg(unix)]
    #[test]
    fn owned_group_stream_bounds_stderr_and_cleans_up_on_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("streamed");
        let mut output_file = File::create(&output_path).unwrap();
        let output = stream_stdout_to_writer_limited_in_owned_group(
            Command::new("sh").args(["-c", "printf data; printf 12345 >&2"]),
            Duration::from_secs(2),
            16,
            4,
            &mut output_file,
        )
        .unwrap();
        assert!(matches!(output, LimitedStreamOutput::StderrLimit));
        drop(output_file);
        assert_eq!(std::fs::read(&output_path).unwrap(), b"data");

        let mut output_file = File::create(&output_path).unwrap();
        let started = Instant::now();
        let output = stream_stdout_to_writer_limited_in_owned_group(
            Command::new("sh").args(["-c", "printf before; sleep 2"]),
            Duration::from_millis(100),
            64,
            16,
            &mut output_file,
        )
        .unwrap();
        assert!(matches!(output, LimitedStreamOutput::Deadline));
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(output_file);
        assert_eq!(std::fs::read(output_path).unwrap(), b"before");
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
