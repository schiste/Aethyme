//! The supervisor's own log (#1521).
//!
//! `env supervise` runs detached with stdout and stderr on `supervisor.log`.
//! Everything it prints, its own lines and every `warning:` from code it
//! calls, goes through [`stamp_output`]: each line gets an RFC 3339 timestamp,
//! and a line repeated within [`REPEAT_WINDOW`] is written once and then
//! summarised (`(repeated 8 more times in 10m)`), so a warning that recurs on
//! every tick neither floods the log nor hides when it started.

use std::collections::HashMap;
use std::time::Duration;

/// Identical lines within this window after the first are counted, not
/// written.
pub const REPEAT_WINDOW: Duration = Duration::from_secs(600);
/// Distinct lines tracked at once; the oldest is forgotten beyond this.
const MAX_TRACKED: usize = 512;

struct Seen {
    first_at_ms: i64,
    suppressed: u64,
}

/// Decides which lines to write. Pure: the caller supplies the clock.
pub struct Throttle {
    window_ms: i64,
    seen: HashMap<String, Seen>,
}

impl Throttle {
    pub fn new(window: Duration) -> Self {
        Self {
            window_ms: window.as_millis() as i64,
            seen: HashMap::new(),
        }
    }

    /// The lines to write for `line` arriving at `now_ms`: summaries of
    /// lines whose window has closed, then `line` unless it repeats one still
    /// inside its window.
    pub fn admit(&mut self, line: &str, now_ms: i64) -> Vec<String> {
        let mut out = self.expire(now_ms);
        match self.seen.get_mut(line) {
            Some(seen) => seen.suppressed += 1,
            None => {
                if self.seen.len() >= MAX_TRACKED
                    && let Some(oldest) = self
                        .seen
                        .iter()
                        .min_by_key(|(_, seen)| seen.first_at_ms)
                        .map(|(line, _)| line.clone())
                    && let Some(seen) = self.seen.remove(&oldest)
                    && seen.suppressed > 0
                {
                    out.push(self.summary(&oldest, &seen));
                }
                self.seen.insert(
                    line.to_string(),
                    Seen {
                        first_at_ms: now_ms,
                        suppressed: 0,
                    },
                );
                out.push(line.to_string());
            }
        }
        out
    }

    /// Summaries for every line still being counted (at exit).
    pub fn flush(&mut self) -> Vec<String> {
        let mut entries: Vec<(String, Seen)> = self.seen.drain().collect();
        entries.sort_by_key(|(_, seen)| seen.first_at_ms);
        entries
            .iter()
            .filter(|(_, seen)| seen.suppressed > 0)
            .map(|(line, seen)| self.summary(line, seen))
            .collect()
    }

    fn expire(&mut self, now_ms: i64) -> Vec<String> {
        let window = self.window_ms;
        let mut closed: Vec<(String, Seen)> = Vec::new();
        self.seen.retain(|line, seen| {
            if now_ms - seen.first_at_ms < window {
                return true;
            }
            closed.push((
                line.clone(),
                Seen {
                    first_at_ms: seen.first_at_ms,
                    suppressed: seen.suppressed,
                },
            ));
            false
        });
        closed.sort_by_key(|(_, seen)| seen.first_at_ms);
        closed
            .iter()
            .filter(|(_, seen)| seen.suppressed > 0)
            .map(|(line, seen)| self.summary(line, seen))
            .collect()
    }

    fn summary(&self, line: &str, seen: &Seen) -> String {
        format!(
            "{line} (repeated {} more time{} in {}m)",
            seen.suppressed,
            if seen.suppressed == 1 { "" } else { "s" },
            (self.window_ms / 60_000).max(1)
        )
    }
}

/// `<RFC 3339 time> <line>`.
pub fn stamp(line: &str, now_ms: i64) -> String {
    format!("{} {line}", crate::cli::insights::timestamp_millis(now_ms))
}

#[cfg(unix)]
pub use unix::{StampedOutput, stamp_output};

#[cfg(unix)]
mod unix {
    use std::fs::File;
    use std::io::{self, BufRead, BufReader, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{REPEAT_WINDOW, Throttle, stamp};
    use crate::environments::logs::open_for_append;

    /// How long exit waits for the writer to drain. A descendant that
    /// inherited stdout would hold the pipe open forever; the exit line is
    /// written by then anyway, only pending repeat summaries would be lost.
    const DRAIN_WAIT: Duration = Duration::from_secs(2);

    /// Restores nothing: the process exits after the supervisor loop. Dropping
    /// it closes this process's stdout and stderr so the writer drains, then
    /// waits (bounded) for it, so the exit line and pending summaries reach
    /// the log.
    pub struct StampedOutput {
        log: std::path::PathBuf,
        drained: mpsc::Receiver<io::Result<()>>,
    }

    impl Drop for StampedOutput {
        fn drop(&mut self) {
            // stderr is still the pipe here, so a failed flush is reported.
            crate::warn_unrecorded("flush supervisor stdout", io::stdout().flush());
            crate::warn_unrecorded("flush supervisor stderr", io::stderr().flush());
            if let Ok(null) = File::open("/dev/null") {
                // SAFETY: dup2 onto the standard descriptors with a valid fd;
                // this closes the pipe's last write ends held by this process.
                unsafe {
                    libc::dup2(null.as_raw_fd(), 1);
                    libc::dup2(null.as_raw_fd(), 2);
                }
            }
            match self.drained.recv_timeout(DRAIN_WAIT) {
                Ok(Ok(())) => {}
                // stderr is gone: the log itself is the only place left.
                Ok(Err(error)) => record_writer_failure(&self.log, &error),
                // A descendant still holds the pipe. The exit line was
                // written before this point; only pending summaries are lost.
                Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
            }
        }
    }

    fn record_writer_failure(log: &Path, error: &io::Error) {
        let line = stamp(
            &format!("warning: aethyme supervisor log writer failed: {error}"),
            crate::environments::activity::now_ms(),
        );
        // Best effort: if the log cannot be written either, nothing is left
        // that could report it.
        open_for_append(log)
            .and_then(|mut file| writeln!(file, "{line}"))
            .ok();
    }

    /// Copy every line from the pipe into the log, stamped and throttled.
    fn pump(reader: File, mut file: File) -> io::Result<()> {
        let mut throttle = Throttle::new(REPEAT_WINDOW);
        for line in BufReader::new(reader).lines() {
            let line = line?;
            let now = crate::environments::activity::now_ms();
            for out in throttle.admit(&line, now) {
                writeln!(file, "{}", stamp(&out, now))?;
            }
        }
        let now = crate::environments::activity::now_ms();
        for out in throttle.flush() {
            writeln!(file, "{}", stamp(&out, now))?;
        }
        file.flush()
    }

    /// Route this process's stdout and stderr through a pipe whose reader
    /// timestamps and throttles each line into `log`.
    ///
    /// Children started with inherited stdio would share the pipe; every
    /// environment command gets its own component log or a captured pipe, so
    /// in practice only this process writes here.
    pub fn stamp_output(log: &Path) -> io::Result<StampedOutput> {
        let file = open_for_append(log)?;
        let mut fds = [0; 2];
        // SAFETY: `fds` is a live two-element array for pipe(2) to fill.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let (read, write) = (fds[0], fds[1]);
        // SAFETY: `read` is a fresh descriptor owned only by this File.
        let reader = unsafe { File::from_raw_fd(read) };
        // SAFETY: the read end must not leak into children (component
        // processes would keep the pipe open past this process's exit).
        unsafe {
            libc::fcntl(read, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        // stderr is still the log file here, so a failed flush is reported.
        crate::warn_unrecorded("flush supervisor stdout", io::stdout().flush());
        crate::warn_unrecorded("flush supervisor stderr", io::stderr().flush());
        // SAFETY: dup2 of a valid descriptor onto stdout and stderr, then the
        // original write end is closed; the standard fds now hold the pipe.
        unsafe {
            if libc::dup2(write, 1) < 0 || libc::dup2(write, 2) < 0 {
                let error = io::Error::last_os_error();
                libc::close(write);
                return Err(error);
            }
            libc::close(write);
        }
        let (done, drained) = mpsc::channel();
        std::thread::spawn(move || {
            let result = pump(reader, file);
            // The receiver is gone only once exit stopped waiting for us.
            done.send(result).ok();
        });
        Ok(StampedOutput {
            log: log.to_path_buf(),
            drained,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_line_is_written_once_then_summarised() {
        let mut throttle = Throttle::new(Duration::from_secs(600));
        let warning = "warning: could not serve a proxied connection";
        assert_eq!(throttle.admit(warning, 0), vec![warning.to_string()]);
        for at in 1..=8 {
            assert!(throttle.admit(warning, at * 1000).is_empty());
        }
        assert_eq!(
            throttle.admit("redeployed baseline", 9_000),
            vec!["redeployed baseline".to_string()],
            "a different line is written at once"
        );
        let after = throttle.admit(warning, 600_001);
        assert_eq!(
            after,
            vec![
                format!("{warning} (repeated 8 more times in 10m)"),
                warning.to_string()
            ],
            "the summary closes the window and the next occurrence is written"
        );
    }

    #[test]
    fn exit_flushes_pending_summaries() {
        let mut throttle = Throttle::new(Duration::from_secs(600));
        throttle.admit("warning: x", 0);
        throttle.admit("warning: x", 1);
        throttle.admit("once", 2);
        assert_eq!(
            throttle.flush(),
            vec!["warning: x (repeated 1 more time in 10m)".to_string()]
        );
    }

    #[test]
    fn lines_are_stamped_in_rfc_3339() {
        assert_eq!(
            stamp("started", 1_794_096_000_123),
            "2026-11-08T00:00:00.123Z started"
        );
    }
}
