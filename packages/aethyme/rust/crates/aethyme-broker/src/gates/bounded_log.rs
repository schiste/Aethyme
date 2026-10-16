//! Bounded disk capture for gate stdout/stderr.
//!
//! The collector retains an output prefix and a rolling tail. It keeps only a
//! fixed-size tail and classifier state in memory, while continuing to drain
//! both pipes after the on-disk prefix is full.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(super) const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;
pub(super) const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Serialize a broker-owned header value without constructing an unbounded
/// intermediate string. The JSON writer keeps only the bounded prefix; the
/// normal log header writer will add its truncation marker if needed.
pub(super) fn bounded_json<T: serde::Serialize>(value: &T) -> io::Result<(Vec<u8>, bool)> {
    let mut writer = CappedHeaderBuffer {
        bytes: Vec::with_capacity(MAX_HEADER_BYTES),
        truncated: false,
    };
    serde_json::to_writer(&mut writer, value).map_err(io::Error::other)?;
    Ok((writer.bytes, writer.truncated))
}

struct CappedHeaderBuffer {
    bytes: Vec<u8>,
    truncated: bool,
}

impl Write for CappedHeaderBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let room = MAX_HEADER_BYTES.saturating_sub(self.bytes.len());
        let keep = bytes.len().min(room);
        self.bytes.extend_from_slice(&bytes[..keep]);
        self.truncated |= keep < bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_NOTE_BYTES: usize = 16 * 1024;
const MAX_TAIL_BYTES: usize = 512 * 1024;
const FOOTER_RESERVE_BYTES: usize = 4 * 1024;
const FINAL_NOTE_RESERVE_BYTES: usize = 2 * 1024;
const PREFIX_BUDGET_BYTES: usize = MAX_LOG_BYTES as usize
    - MAX_HEADER_BYTES
    - MAX_NOTE_BYTES
    - MAX_TAIL_BYTES
    - FOOTER_RESERVE_BYTES
    - FINAL_NOTE_RESERVE_BYTES;
const WRITE_BUFFER_BYTES: usize = 32 * 1024;
const READ_BUFFER_BYTES: usize = 8 * 1024;
const POLL_INTERVAL_MS: i32 = 50;
const MAX_PATTERN_LEN: usize = 32;

const CLASSIFIER_PATTERNS: &[&str] = &[
    "timed out",
    "timeout exceeded",
    "command timed out",
    "no space left on device",
    "database is locked",
    "resource busy",
    "resource temporarily unavailable",
    "text file busy",
    "too many open files",
    ".fingerprint",
    "target/debug",
    "target/release",
    ".rlib",
    "build directory",
    "extern location",
    "no such file or directory",
    "failed to lock",
    "failed to acquire",
    "failed to rename",
    "failed to remove",
    "failed to write",
    "file exists",
    ": command not found",
    "command not found",
    "not found on path",
    "executable file not found",
    "permission denied",
    "cannot execute",
    "could not compile",
];

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct CaptureSummary {
    pub output_bytes: u64,
    pub incomplete: bool,
}

/// One shared append-only log sink for the two pipe readers and broker notes.
pub(super) struct BoundedGateLog {
    file: File,
    file_bytes: usize,
    header_bytes: usize,
    note_bytes: usize,
    prefix_buffer: Vec<u8>,
    output_prefix_bytes: usize,
    output_bytes: u64,
    tail: VecDeque<u8>,
    matcher: PatternMatcher,
    write_error: Option<String>,
    finished: bool,
}

impl BoundedGateLog {
    pub(super) fn create(path: &Path) -> io::Result<Self> {
        Ok(Self {
            file: File::create(path)?,
            file_bytes: 0,
            header_bytes: 0,
            note_bytes: 0,
            prefix_buffer: Vec::with_capacity(WRITE_BUFFER_BYTES),
            output_prefix_bytes: 0,
            output_bytes: 0,
            tail: VecDeque::with_capacity(MAX_TAIL_BYTES),
            matcher: PatternMatcher::default(),
            write_error: None,
            finished: false,
        })
    }

    pub(super) fn header_remaining(&self) -> usize {
        MAX_HEADER_BYTES.saturating_sub(self.header_bytes)
    }

    pub(super) fn write_header(&mut self, bytes: &[u8]) -> io::Result<()> {
        const TRUNCATED: &[u8] = b"aethyme: gate log header truncated at 64 KiB\n";
        let remaining = MAX_HEADER_BYTES.saturating_sub(self.header_bytes);
        let truncated = bytes.len() > remaining;
        let keep = if truncated {
            remaining.saturating_sub(TRUNCATED.len()).min(bytes.len())
        } else {
            bytes.len()
        };
        self.write_file_bytes(&bytes[..keep])?;
        self.header_bytes += keep;
        if truncated {
            let marker_bytes = remaining.saturating_sub(keep).min(TRUNCATED.len());
            self.write_file_bytes(&TRUNCATED[..marker_bytes])?;
            self.header_bytes += marker_bytes;
        }
        Ok(())
    }

    /// Append a bounded broker diagnostic while the child may still be alive.
    pub(super) fn write_note(&mut self, bytes: &[u8]) {
        self.flush_prefix_buffer();
        if self.write_error.is_some() || self.note_bytes >= MAX_NOTE_BYTES {
            return;
        }
        let available = MAX_NOTE_BYTES - self.note_bytes;
        let keep = bytes.len().min(available);
        self.try_write_file_bytes(&bytes[..keep]);
        self.note_bytes += keep;
    }

    pub(super) fn record_output(&mut self, bytes: &[u8]) {
        self.output_bytes = self.output_bytes.saturating_add(bytes.len() as u64);
        self.matcher.feed(bytes);
        let mut overflow = bytes;
        if self.write_error.is_none() {
            let output_room = PREFIX_BUDGET_BYTES.saturating_sub(self.output_prefix_bytes);
            let keep = bytes.len().min(output_room);
            if keep > 0 {
                self.prefix_buffer.extend_from_slice(&bytes[..keep]);
                self.output_prefix_bytes += keep;
                overflow = &bytes[keep..];
                if self.prefix_buffer.len() >= WRITE_BUFFER_BYTES {
                    self.flush_prefix_buffer();
                }
            }
        }
        if !overflow.is_empty() {
            self.push_tail(overflow);
        }
    }

    fn push_tail(&mut self, bytes: &[u8]) {
        if bytes.len() >= MAX_TAIL_BYTES {
            self.tail.clear();
            self.tail.extend(&bytes[bytes.len() - MAX_TAIL_BYTES..]);
            return;
        }
        self.tail.extend(bytes.iter().copied());
        while self.tail.len() > MAX_TAIL_BYTES {
            self.tail.pop_front();
        }
    }

    fn flush_prefix_buffer(&mut self) {
        if self.prefix_buffer.is_empty() || self.write_error.is_some() {
            self.prefix_buffer.clear();
            return;
        }
        let bytes = std::mem::take(&mut self.prefix_buffer);
        self.try_write_file_bytes(&bytes);
        self.prefix_buffer = Vec::with_capacity(WRITE_BUFFER_BYTES);
    }

    fn try_write_file_bytes(&mut self, bytes: &[u8]) {
        if bytes.is_empty() || self.write_error.is_some() {
            return;
        }
        let room = (MAX_LOG_BYTES as usize).saturating_sub(self.file_bytes);
        let keep = bytes.len().min(room);
        if keep == 0 {
            return;
        }
        match self.file.write_all(&bytes[..keep]) {
            Ok(()) => self.file_bytes = self.file_bytes.saturating_add(keep),
            Err(error) => self.write_error = Some(error.to_string()),
        }
    }

    fn write_file_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() > MAX_LOG_BYTES as usize - self.file_bytes {
            return Err(io::Error::other("bounded gate log header exceeds file cap"));
        }
        self.file.write_all(bytes)?;
        self.file_bytes = self.file_bytes.saturating_add(bytes.len());
        Ok(())
    }

    pub(super) fn finish(&mut self) -> CaptureSummary {
        if self.finished {
            return CaptureSummary {
                output_bytes: self.output_bytes,
                incomplete: self.write_error.is_some(),
            };
        }
        self.finished = true;
        self.flush_prefix_buffer();
        let truncated = self.output_bytes > self.output_prefix_bytes as u64;
        if truncated && self.write_error.is_none() {
            let tail_bytes = self.tail.len();
            let omitted = self
                .output_bytes
                .saturating_sub(self.output_prefix_bytes as u64)
                .saturating_sub(tail_bytes as u64);
            let mut footer = format!(
                "\naethyme: gate output capture truncated; observed {} bytes; retained {} prefix and {} tail bytes; omitted {} middle bytes\n",
                self.output_bytes, self.output_prefix_bytes, tail_bytes, omitted
            );
            let matched = self.matcher.matched_patterns();
            if !matched.is_empty() {
                footer.push_str("aethyme: classifier evidence observed in complete gate stream: ");
                footer.push_str(&matched.join("; "));
                footer.push('\n');
            }
            footer.truncate(footer.len().min(FOOTER_RESERVE_BYTES));
            self.try_write_file_bytes(footer.as_bytes());
            // The reserved space guarantees the complete tail fits unless an
            // earlier filesystem error made capture incomplete.
            if self.write_error.is_none() {
                let tail = self.tail.iter().copied().collect::<Vec<_>>();
                self.try_write_file_bytes(&tail);
            }
        }
        if self.write_error.is_none() {
            if let Err(error) = self.file.flush() {
                self.write_error = Some(error.to_string());
            }
        }
        CaptureSummary {
            output_bytes: self.output_bytes,
            incomplete: self.write_error.is_some(),
        }
    }

    pub(super) fn append_note_after_finish(&mut self, bytes: &[u8]) {
        if !self.finished {
            self.write_note(bytes);
            return;
        }
        let reserve = FINAL_NOTE_RESERVE_BYTES.min(bytes.len());
        self.try_write_file_bytes(&bytes[..reserve]);
    }

    pub(super) fn write_error(&self) -> Option<&str> {
        self.write_error.as_deref()
    }
}

#[derive(Default)]
struct PatternMatcher {
    carry: Vec<u8>,
    matched: u64,
}

impl PatternMatcher {
    fn feed(&mut self, bytes: &[u8]) {
        let mut window = Vec::with_capacity(self.carry.len() + bytes.len());
        window.extend_from_slice(&self.carry);
        window.extend(bytes.iter().map(u8::to_ascii_lowercase));
        for (index, pattern) in CLASSIFIER_PATTERNS.iter().enumerate() {
            let bit = 1u64 << index;
            if self.matched & bit == 0 && contains_bytes(&window, pattern.as_bytes()) {
                self.matched |= bit;
            }
        }
        let keep = window.len().min(MAX_PATTERN_LEN - 1);
        self.carry.clear();
        self.carry
            .extend_from_slice(&window[window.len().saturating_sub(keep)..]);
    }

    fn matched_patterns(&self) -> Vec<&'static str> {
        CLASSIFIER_PATTERNS
            .iter()
            .enumerate()
            .filter_map(|(index, pattern)| {
                (self.matched & (1u64 << index) != 0).then_some(*pattern)
            })
            .collect()
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

pub(super) fn set_nonblocking<R: AsRawFd>(reader: &R) -> io::Result<()> {
    let fd = reader.as_raw_fd();
    // SAFETY: fcntl reads and updates flags on this live pipe descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: O_NONBLOCK ensures a readiness race cannot leave the worker
    // stuck in read after the bounded drain deadline.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn drain_pipe<R: Read + AsRawFd + Send + 'static>(
    mut reader: R,
    collector: Arc<Mutex<BoundedGateLog>>,
    stop: Arc<AtomicBool>,
    first_output_ms: Arc<AtomicI64>,
    drain_failed: Arc<AtomicBool>,
    drain_error: Arc<Mutex<Option<String>>>,
    started: Instant,
) -> io::Result<()> {
    let fd = reader.as_raw_fd();
    let mut buffer = [0u8; READ_BUFFER_BYTES];
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(());
        }
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        };
        // SAFETY: poll receives a valid pointer to one initialized pollfd.
        let polled = unsafe { libc::poll(&mut pollfd, 1, POLL_INTERVAL_MS) };
        if polled < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            record_drain_error(&drain_failed, &drain_error, &error);
            return Err(error);
        }
        if polled == 0 {
            continue;
        }
        loop {
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            match reader.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(count) => {
                    let _ = first_output_ms.compare_exchange(
                        -1,
                        started.elapsed().as_millis().min(i64::MAX as u128) as i64,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                    let mut state = collector
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.record_output(&buffer[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    record_drain_error(&drain_failed, &drain_error, &error);
                    return Err(error);
                }
            }
        }
    }
}

fn record_drain_error(failed: &AtomicBool, slot: &Mutex<Option<String>>, error: &io::Error) {
    if let Ok(mut message) = slot.lock() {
        if message.is_none() {
            *message = Some(error.to_string());
        }
    }
    failed.store(true, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_bounded_prefix_tail_and_classifier_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gate.log");
        let mut log = BoundedGateLog::create(&path).unwrap();
        log.write_header(b"header\n").unwrap();
        let mut output = vec![b'a'; PREFIX_BUDGET_BYTES + 4096];
        let evidence_at = PREFIX_BUDGET_BYTES + 32;
        output[evidence_at..evidence_at + 24].copy_from_slice(b"no space left on device");
        output.extend(std::iter::repeat_n(b'z', MAX_TAIL_BYTES));
        log.record_output(&output);
        let summary = log.finish();

        assert_eq!(summary.output_bytes, output.len() as u64);
        assert!(!summary.incomplete);
        let saved = std::fs::read(&path).unwrap();
        assert!(saved.len() <= MAX_LOG_BYTES as usize);
        let saved = String::from_utf8_lossy(&saved);
        assert!(saved.starts_with("header\n"));
        assert!(saved.contains("observed "));
        assert!(saved.contains("omitted "));
        assert!(saved.contains("no space left on device"));
        assert!(super::super::is_resource_contention_error(
            "cargo test",
            &path
        ));
        assert!(saved.ends_with(&"z".repeat(MAX_TAIL_BYTES)));
    }

    #[test]
    fn timeout_notes_and_final_capture_notes_keep_the_file_below_the_cap() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gate.log");
        let mut log = BoundedGateLog::create(&path).unwrap();
        log.write_header(b"header\n").unwrap();
        log.write_note(&vec![b't'; MAX_NOTE_BYTES * 2]);
        log.record_output(&vec![b'o'; PREFIX_BUDGET_BYTES + 1]);
        let summary = log.finish();
        log.append_note_after_finish(&vec![b'n'; FINAL_NOTE_RESERVE_BYTES * 2]);

        assert_eq!(summary.output_bytes, (PREFIX_BUDGET_BYTES + 1) as u64);
        assert!(std::fs::metadata(path).unwrap().len() <= MAX_LOG_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn stdout_and_stderr_drainers_share_the_bounded_collector() {
        use std::io::Write as _;
        use std::os::unix::net::UnixStream;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gate.log");
        let collector = Arc::new(Mutex::new(BoundedGateLog::create(&path).unwrap()));
        let stop = Arc::new(AtomicBool::new(false));
        let first_output = Arc::new(AtomicI64::new(-1));
        let failed = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let started = Instant::now();
        let (stdout_read, mut stdout_write) = UnixStream::pair().unwrap();
        let (stderr_read, mut stderr_write) = UnixStream::pair().unwrap();
        set_nonblocking(&stdout_read).unwrap();
        set_nonblocking(&stderr_read).unwrap();

        std::thread::scope(|scope| {
            let stdout = scope.spawn({
                let collector = collector.clone();
                let stop = stop.clone();
                let first_output = first_output.clone();
                let failed = failed.clone();
                let error = error.clone();
                move || {
                    drain_pipe(
                        stdout_read,
                        collector,
                        stop,
                        first_output,
                        failed,
                        error,
                        started,
                    )
                }
            });
            let stderr = scope.spawn({
                let collector = collector.clone();
                let stop = stop.clone();
                let first_output = first_output.clone();
                let failed = failed.clone();
                let error = error.clone();
                move || {
                    drain_pipe(
                        stderr_read,
                        collector,
                        stop,
                        first_output,
                        failed,
                        error,
                        started,
                    )
                }
            });
            stdout_write.write_all(b"stdout bytes\n").unwrap();
            stderr_write.write_all(b"stderr bytes\n").unwrap();
            drop(stdout_write);
            drop(stderr_write);
            stdout.join().unwrap().unwrap();
            stderr.join().unwrap().unwrap();
        });

        let summary = collector.lock().unwrap().finish();
        let saved = std::fs::read_to_string(path).unwrap();
        assert_eq!(summary.output_bytes, 26);
        assert!(saved.contains("stdout bytes\n"));
        assert!(saved.contains("stderr bytes\n"));
        assert!(first_output.load(Ordering::Acquire) >= 0);
        assert!(!failed.load(Ordering::Acquire));
        assert!(error.lock().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn pipe_reader_can_be_cancelled_when_a_descendant_keeps_its_pipe_open() {
        use std::os::unix::net::UnixStream;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gate.log");
        let collector = Arc::new(Mutex::new(BoundedGateLog::create(&path).unwrap()));
        let stop = Arc::new(AtomicBool::new(false));
        let first_output = Arc::new(AtomicI64::new(-1));
        let failed = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let (reader, _writer) = UnixStream::pair().unwrap();
        set_nonblocking(&reader).unwrap();
        let handle = std::thread::spawn({
            let collector = collector.clone();
            let stop = stop.clone();
            let first_output = first_output.clone();
            let failed = failed.clone();
            let error = error.clone();
            move || {
                drain_pipe(
                    reader,
                    collector,
                    stop,
                    first_output,
                    failed,
                    error,
                    Instant::now(),
                )
            }
        });
        std::thread::sleep(Duration::from_millis(80));
        assert!(!handle.is_finished(), "the writer keeps the pipe open");
        stop.store(true, Ordering::Release);
        handle.join().unwrap().unwrap();
        assert!(!failed.load(Ordering::Acquire));
    }

    #[test]
    fn classifier_match_can_span_pipe_chunks() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gate.log");
        let mut log = BoundedGateLog::create(&path).unwrap();
        log.record_output(b"prefix no space left on ");
        log.record_output(b"device suffix");
        let summary = log.finish();
        assert_eq!(summary.output_bytes, 37);
        let saved = std::fs::read_to_string(path).unwrap();
        assert!(saved.contains("no space left on device"));
    }
}
