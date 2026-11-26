//! The last bytes of each preparation step's output, kept for diagnosis.
//!
//! A step's stdout and stderr used to go straight to the terminal of whoever
//! ran `prepare` and nowhere else, so a failed or interrupted install could
//! not be attributed from any log. Each stream now passes through a pipe: the
//! bytes are forwarded to the broker's own stdout and stderr as before, and
//! the last [`TAIL_BYTES`] of each are kept. After every step the tails are
//! written to one bounded log ([`LOG_MAX_BYTES`]) in the session's scratch
//! directory, which is retired with the session, or beside the session's
//! preparation record when it has none. Failure messages name the log.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// Bytes kept from the end of each stream of one step.
pub(crate) const TAIL_BYTES: usize = 16 * 1024;
/// The log's largest size; the oldest steps' sections are dropped first.
pub(crate) const LOG_MAX_BYTES: usize = 64 * 1024;
/// The log's name in the session's scratch directory.
pub(crate) const LOG_FILE: &str = "preparation-output.log";
/// How long a finished step waits for its streams to drain. A descendant
/// that keeps the pipe open does not hold the preparation up.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// The last `limit` bytes written to a stream, and how many were written.
#[derive(Debug)]
pub(crate) struct Tail {
    bytes: VecDeque<u8>,
    seen: u64,
    limit: usize,
}

impl Tail {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            bytes: VecDeque::with_capacity(limit.min(4096)),
            seen: 0,
            limit,
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.seen = self.seen.saturating_add(chunk.len() as u64);
        let keep = &chunk[chunk.len().saturating_sub(self.limit)..];
        self.bytes.extend(keep);
        let excess = self.bytes.len().saturating_sub(self.limit);
        self.bytes.drain(..excess);
    }

    pub(crate) fn contents(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    /// Whether earlier bytes were dropped.
    pub(crate) fn truncated(&self) -> bool {
        self.seen > self.bytes.len() as u64
    }
}

/// One step's captured streams.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct StepOutput {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stdout_truncated: bool,
    pub(crate) stderr: Vec<u8>,
    pub(crate) stderr_truncated: bool,
}

/// The broker's side of a step's captured streams.
pub(crate) struct Capture {
    stdout: Arc<Mutex<Tail>>,
    stderr: Arc<Mutex<Tail>>,
    drained: mpsc::Receiver<()>,
    readers: usize,
}

/// The pipe ends a supervised step writes to.
pub(crate) struct CaptureEnds {
    pub(crate) stdout: std::process::Stdio,
    pub(crate) stderr: std::process::Stdio,
}

#[derive(Clone, Copy)]
enum Forward {
    Stdout,
    Stderr,
}

/// Open both pipes and start forwarding them.
pub(crate) fn start() -> std::io::Result<(Capture, CaptureEnds)> {
    let (stdout_reader, stdout_writer) = std::io::pipe()?;
    let (stderr_reader, stderr_writer) = std::io::pipe()?;
    let stdout = Arc::new(Mutex::new(Tail::new(TAIL_BYTES)));
    let stderr = Arc::new(Mutex::new(Tail::new(TAIL_BYTES)));
    let (sender, drained) = mpsc::channel();
    let mut readers = 0;
    for (reader, tail, forward) in [
        (stdout_reader, Arc::clone(&stdout), Forward::Stdout),
        (stderr_reader, Arc::clone(&stderr), Forward::Stderr),
    ] {
        let sender = sender.clone();
        std::thread::Builder::new()
            .name("prepare-step-output".into())
            .spawn(move || forward_stream(reader, &tail, forward, &sender))?;
        readers += 1;
    }
    Ok((
        Capture {
            stdout,
            stderr,
            drained,
            readers,
        },
        CaptureEnds {
            stdout: stdout_writer.into(),
            stderr: stderr_writer.into(),
        },
    ))
}

fn forward_stream(
    mut reader: std::io::PipeReader,
    tail: &Mutex<Tail>,
    forward: Forward,
    drained: &mpsc::Sender<()>,
) {
    let mut buffer = [0_u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let chunk = &buffer[..read];
                // Forwarding is what the inherited descriptor used to do; a
                // closed terminal must not stop the capture or the step.
                match forward {
                    Forward::Stdout => std::io::stdout().write_all(chunk).ok(),
                    Forward::Stderr => std::io::stderr().write_all(chunk).ok(),
                };
                if let Ok(mut tail) = tail.lock() {
                    tail.push(chunk);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    // The receiver may have stopped waiting after its grace period.
    drained.send(()).ok();
}

impl Capture {
    /// The tails once both streams ended, or after [`DRAIN_GRACE`].
    pub(crate) fn finish(self) -> StepOutput {
        let deadline = Instant::now() + DRAIN_GRACE;
        for _ in 0..self.readers {
            let left = deadline.saturating_duration_since(Instant::now());
            if self.drained.recv_timeout(left).is_err() {
                crate::phase_telemetry::add(
                    "preparation_step_output_drain_timed_out",
                    Duration::ZERO,
                );
                break;
            }
        }
        let snapshot = |tail: &Mutex<Tail>| {
            tail.lock()
                .map(|tail| (tail.contents(), tail.truncated()))
                .unwrap_or_default()
        };
        let (stdout, stdout_truncated) = snapshot(&self.stdout);
        let (stderr, stderr_truncated) = snapshot(&self.stderr);
        StepOutput {
            stdout,
            stdout_truncated,
            stderr,
            stderr_truncated,
        }
    }
}

/// One step's section of the log.
pub(crate) fn render_section(step: &str, outcome: &str, output: &StepOutput) -> String {
    let stream = |name: &str, bytes: &[u8], truncated: bool| {
        let mut text = format!(
            "--- {name}{} ---\n",
            if truncated {
                format!(" (last {} bytes)", bytes.len())
            } else {
                String::new()
            }
        );
        text.push_str(&String::from_utf8_lossy(bytes));
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text
    };
    format!(
        "=== step {step:?}: {outcome} ===\n{}{}",
        stream("stdout", &output.stdout, output.stdout_truncated),
        stream("stderr", &output.stderr, output.stderr_truncated)
    )
}

/// `previous` with `section` appended, keeping at most `max` bytes by
/// dropping whole leading sections (or, for one oversized section, its
/// beginning at a character boundary).
pub(crate) fn bounded_log(previous: &str, section: &str, max: usize) -> String {
    let mut log = format!("{previous}{section}");
    while log.len() > max {
        // Search past the first character, so the section at the front is
        // never found again.
        let skip = log.chars().next().map_or(0, char::len_utf8);
        match log[skip..].find("=== step ") {
            Some(next) => {
                log.drain(..next + skip);
            }
            None => {
                let mut cut = log.len() - max;
                while !log.is_char_boundary(cut) {
                    cut += 1;
                }
                log.drain(..cut);
            }
        }
    }
    log
}

/// Where a session's step log goes: its scratch directory, which is retired
/// with it, or beside its preparation record.
pub(crate) fn log_path(scratch: Option<&Path>, main_root: &Path, session_id: i64) -> PathBuf {
    match scratch {
        Some(scratch) => scratch.join(LOG_FILE),
        None => main_root
            .join(".aethyme/run/preparation")
            .join(format!("session-{session_id}.output.log")),
    }
}

/// Write `log` to `path` through a temporary file in the same directory.
pub(crate) fn write_log(path: &Path, log: &str) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("step output log has no parent directory"))?;
    std::fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".preparation-output.{}.tmp", std::process::id()));
    std::fs::write(&temp, log)?;
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tail_keeps_the_last_bytes_and_knows_it_dropped_some() {
        let mut tail = Tail::new(8);
        tail.push(b"abc");
        assert_eq!(tail.contents(), b"abc");
        assert!(!tail.truncated());
        tail.push(b"defghij");
        assert_eq!(tail.contents(), b"cdefghij");
        assert!(tail.truncated());
        tail.push(b"0123456789ABCDEF");
        assert_eq!(tail.contents(), b"89ABCDEF");
    }

    #[test]
    fn the_log_drops_whole_old_sections_first() {
        let output = StepOutput {
            stdout: b"installed 1200 packages".to_vec(),
            stdout_truncated: false,
            stderr: b"ERR_PNPM_ENOSPC".to_vec(),
            stderr_truncated: true,
        };
        let first = render_section("deps", "failed", &output);
        assert!(
            first.starts_with("=== step \"deps\": failed ===\n"),
            "{first}"
        );
        assert!(first.contains("--- stderr (last 15 bytes) ---\nERR_PNPM_ENOSPC\n"));
        let second = render_section("build", "ok", &StepOutput::default());
        let log = bounded_log(&first, &second, second.len() + 1);
        assert_eq!(log, second, "the older section went whole");
        let both = bounded_log(&first, &second, 10_000);
        assert_eq!(both, format!("{first}{second}"));
        let cut = bounded_log("", &"é".repeat(100), 51);
        assert!(cut.len() <= 51 && cut.chars().all(|c| c == 'é'));
    }

    #[test]
    fn the_log_lives_in_the_scratch_directory_when_there_is_one() {
        let main = Path::new("/repo");
        assert_eq!(
            log_path(Some(Path::new("/scratch")), main, 7),
            Path::new("/scratch").join(LOG_FILE)
        );
        assert_eq!(
            log_path(None, main, 7),
            Path::new("/repo/.aethyme/run/preparation/session-7.output.log")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_child_writing_to_the_pipes_is_captured() {
        let (capture, ends) = start().unwrap();
        let status = std::process::Command::new("sh")
            .args(["-c", "printf out; printf err >&2"])
            .stdout(ends.stdout)
            .stderr(ends.stderr)
            .status()
            .unwrap();
        assert!(status.success());
        let output = capture.finish();
        assert_eq!(output.stdout, b"out");
        assert_eq!(output.stderr, b"err");
    }

    #[test]
    fn the_log_is_written_whole() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested").join(LOG_FILE);
        write_log(&path, "first").unwrap();
        write_log(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
    }
}
