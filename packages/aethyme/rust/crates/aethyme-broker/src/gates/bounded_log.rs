//! Bounded disk capture for gate stdout/stderr.
//!
//! The collector retains an output prefix and a rolling tail. It keeps only a
//! fixed-size tail and classifier state in memory, while continuing to drain
//! both pipes after the on-disk prefix is full.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read, Seek, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::Path;
#[cfg(unix)]
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub(crate) const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;
pub(crate) const AGENT_LOG_MAX_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Serialize a broker-owned header value without constructing an unbounded
/// intermediate string. The JSON writer keeps only the bounded prefix; the
/// normal log header writer will add its truncation marker if needed.
pub(crate) fn bounded_json<T: serde::Serialize>(value: &T) -> io::Result<(Vec<u8>, bool)> {
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
pub(crate) const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_NOTE_BYTES: usize = 16 * 1024;
const MAX_TAIL_BYTES: usize = 512 * 1024;
const FOOTER_RESERVE_BYTES: usize = 4 * 1024;
const FINAL_NOTE_RESERVE_BYTES: usize = 2 * 1024;
#[cfg(test)]
const PREFIX_BUDGET_BYTES: usize = MAX_LOG_BYTES as usize
    - MAX_HEADER_BYTES
    - MAX_NOTE_BYTES
    - MAX_TAIL_BYTES
    - FOOTER_RESERVE_BYTES
    - FINAL_NOTE_RESERVE_BYTES;
const WRITE_BUFFER_BYTES: usize = 32 * 1024;
#[cfg(unix)]
const READ_BUFFER_BYTES: usize = 8 * 1024;
#[cfg(unix)]
const POLL_INTERVAL_MS: i32 = 50;

const NON_CACHEABLE_TEST_TIMEOUT_PATTERNS: &[&str] =
    &["timed out", "timeout exceeded", "command timed out"];
const TEST_TIMEOUT_EVIDENCE_MARKER: &[u8] = b"aethyme: test timeout text observed in complete ";

const CLASSIFIER_PATTERNS: &[&str] = &[
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

pub(crate) fn has_test_timeout_text(path: &Path) -> bool {
    let Ok(mut bytes) = std::fs::read(path) else {
        return false;
    };
    bytes.make_ascii_lowercase();

    NON_CACHEABLE_TEST_TIMEOUT_PATTERNS.iter().any(|pattern| {
        bytes
            .windows(pattern.len())
            .any(|window| window == pattern.as_bytes())
    }) || bytes
        .windows(TEST_TIMEOUT_EVIDENCE_MARKER.len())
        .any(|window| window == TEST_TIMEOUT_EVIDENCE_MARKER)
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CaptureSummary {
    pub output_bytes: u64,
    pub incomplete: bool,
}

/// One shared append-only log sink for the two pipe readers and broker notes.
pub(crate) struct BoundedGateLog {
    file: File,
    file_bytes: usize,
    max_log_bytes: usize,
    prefix_budget_bytes: usize,
    output_kind: &'static str,
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
    pub(crate) fn create(path: &Path) -> io::Result<Self> {
        Self::create_with_cap(path, MAX_LOG_BYTES, "gate")
    }

    pub(crate) fn create_with_cap(
        path: &Path,
        max_log_bytes: u64,
        output_kind: &'static str,
    ) -> io::Result<Self> {
        Self::from_file(File::create(path)?, max_log_bytes, output_kind)
    }

    pub(crate) fn from_file(
        mut file: File,
        max_log_bytes: u64,
        output_kind: &'static str,
    ) -> io::Result<Self> {
        let max_log_bytes = usize::try_from(max_log_bytes)
            .map_err(|_| io::Error::other("bounded log cap does not fit this platform"))?;
        let reserved_bytes = MAX_HEADER_BYTES
            .saturating_add(MAX_NOTE_BYTES)
            .saturating_add(MAX_TAIL_BYTES)
            .saturating_add(FOOTER_RESERVE_BYTES)
            .saturating_add(FINAL_NOTE_RESERVE_BYTES);
        if max_log_bytes < reserved_bytes {
            return Err(io::Error::other(
                "bounded log cap is smaller than its fixed header, note, tail, and footer reserves",
            ));
        }
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != 0 || file.stream_position()? != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bounded capture requires an empty regular file positioned at its beginning",
            ));
        }
        Ok(Self {
            file,
            file_bytes: 0,
            max_log_bytes,
            prefix_budget_bytes: max_log_bytes - reserved_bytes,
            output_kind,
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

    pub(crate) fn header_remaining(&self) -> usize {
        MAX_HEADER_BYTES.saturating_sub(self.header_bytes)
    }

    pub(crate) fn write_header(&mut self, bytes: &[u8]) -> io::Result<()> {
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
    pub(crate) fn write_note(&mut self, bytes: &[u8]) {
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
            let output_room = self
                .prefix_budget_bytes
                .saturating_sub(self.output_prefix_bytes);
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
        let room = self.max_log_bytes.saturating_sub(self.file_bytes);
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
        if bytes.len() > self.max_log_bytes.saturating_sub(self.file_bytes) {
            return Err(io::Error::other("bounded gate log header exceeds file cap"));
        }
        self.file.write_all(bytes)?;
        self.file_bytes = self.file_bytes.saturating_add(bytes.len());
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> CaptureSummary {
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
                "\naethyme: {} output capture truncated; observed {} bytes; retained {} prefix and {} tail bytes; omitted {} middle bytes\n",
                self.output_kind, self.output_bytes, self.output_prefix_bytes, tail_bytes, omitted
            );
            let matched = self.matcher.matched_patterns();
            if !matched.is_empty() {
                footer.push_str("aethyme: classifier evidence observed in complete ");
                footer.push_str(self.output_kind);
                footer.push_str(" stream: ");
                footer.push_str(&matched.join("; "));
                footer.push('\n');
            }
            if self.matcher.saw_test_timeout_text() {
                footer.push_str("aethyme: test timeout text observed in complete ");
                footer.push_str(self.output_kind);
                footer.push_str(" stream\n");
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
        if self.write_error.is_none()
            && let Err(error) = self.file.flush()
        {
            self.write_error = Some(error.to_string());
        }
        CaptureSummary {
            output_bytes: self.output_bytes,
            incomplete: self.write_error.is_some(),
        }
    }

    pub(crate) fn append_note_after_finish(&mut self, bytes: &[u8]) {
        if !self.finished {
            self.write_note(bytes);
            return;
        }
        let reserve = FINAL_NOTE_RESERVE_BYTES.min(bytes.len());
        self.try_write_file_bytes(&bytes[..reserve]);
    }

    pub(crate) fn write_error(&self) -> Option<&str> {
        self.write_error.as_deref()
    }

    pub(crate) fn sync_data(&mut self) -> io::Result<()> {
        self.flush_prefix_buffer();
        if let Some(error) = &self.write_error {
            return Err(io::Error::other(error.clone()));
        }
        self.file.sync_data()
    }
}

const DFA_COLUMNS: usize = 128;
const MAX_DFA_STATES: usize = 475;
const MAX_TEST_TIMEOUT_DFA_STATES: usize = 64;
const NO_DFA_STATE: u16 = u16::MAX;

struct DfaNode {
    transitions: [u16; DFA_COLUMNS],
    failure: u16,
    outputs: u64,
}

impl DfaNode {
    fn new() -> Self {
        Self {
            transitions: [NO_DFA_STATE; DFA_COLUMNS],
            failure: 0,
            outputs: 0,
        }
    }
}

struct PatternDfa {
    nodes: Vec<DfaNode>,
}

static CLASSIFIER_DFA: OnceLock<PatternDfa> = OnceLock::new();
static NON_CACHEABLE_TEST_TIMEOUT_DFA: OnceLock<PatternDfa> = OnceLock::new();

fn classifier_dfa() -> &'static PatternDfa {
    CLASSIFIER_DFA.get_or_init(|| build_dfa(CLASSIFIER_PATTERNS, MAX_DFA_STATES, "classifier"))
}

fn non_cacheable_test_timeout_dfa() -> &'static PatternDfa {
    NON_CACHEABLE_TEST_TIMEOUT_DFA.get_or_init(|| {
        build_dfa(
            NON_CACHEABLE_TEST_TIMEOUT_PATTERNS,
            MAX_TEST_TIMEOUT_DFA_STATES,
            "non-cacheable test-timeout",
        )
    })
}

fn build_dfa(patterns: &[&str], max_states: usize, kind: &str) -> PatternDfa {
    assert!(
        patterns.len() <= u64::BITS as usize,
        "DFA pattern mask is too small"
    );
    let mut nodes = Vec::with_capacity(max_states);
    nodes.push(DfaNode::new());

    for (pattern_index, pattern) in patterns.iter().enumerate() {
        let mut state = 0usize;
        for byte in pattern.bytes() {
            let column = byte.to_ascii_lowercase() as usize;
            debug_assert!(column < DFA_COLUMNS);
            let next = nodes[state].transitions[column];
            state = if next == NO_DFA_STATE {
                assert!(nodes.len() < max_states, "{kind} DFA state cap exceeded");
                let next = u16::try_from(nodes.len()).expect("pattern DFA fits in u16");
                nodes[state].transitions[column] = next;
                nodes.push(DfaNode::new());
                next as usize
            } else {
                next as usize
            };
        }
        nodes[state].outputs |= 1u64 << pattern_index;
    }

    let mut pending = VecDeque::with_capacity(nodes.len());
    for column in 0..DFA_COLUMNS {
        let next = nodes[0].transitions[column];
        if next == NO_DFA_STATE {
            nodes[0].transitions[column] = 0;
        } else {
            nodes[next as usize].failure = 0;
            pending.push_back(next);
        }
    }

    while let Some(state) = pending.pop_front() {
        let state_index = state as usize;
        let failure_index = nodes[state_index].failure as usize;
        nodes[state_index].outputs |= nodes[failure_index].outputs;

        for column in 0..DFA_COLUMNS {
            let next = nodes[state_index].transitions[column];
            if next == NO_DFA_STATE {
                nodes[state_index].transitions[column] = nodes[failure_index].transitions[column];
            } else {
                nodes[next as usize].failure = nodes[failure_index].transitions[column];
                pending.push_back(next);
            }
        }
    }

    debug_assert!(nodes.len() <= max_states);
    PatternDfa { nodes }
}

#[derive(Default)]
struct PatternMatcher {
    state: u16,
    matched: u64,
    timeout_state: u16,
    saw_test_timeout_text: bool,
}

impl PatternMatcher {
    fn feed(&mut self, bytes: &[u8]) {
        let dfa = classifier_dfa();
        let timeout_dfa = non_cacheable_test_timeout_dfa();
        for byte in bytes {
            let folded = byte.to_ascii_lowercase();
            self.state = if folded.is_ascii() {
                dfa.nodes[self.state as usize].transitions[folded as usize]
            } else {
                0
            };
            self.matched |= dfa.nodes[self.state as usize].outputs;
            self.timeout_state = if folded.is_ascii() {
                timeout_dfa.nodes[self.timeout_state as usize].transitions[folded as usize]
            } else {
                0
            };
            self.saw_test_timeout_text |=
                timeout_dfa.nodes[self.timeout_state as usize].outputs != 0;
        }
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

    fn saw_test_timeout_text(&self) -> bool {
        self.saw_test_timeout_text
    }
}

#[cfg(test)]
mod classifier_tests {
    use super::{
        CLASSIFIER_PATTERNS, DFA_COLUMNS, MAX_DFA_STATES, MAX_TEST_TIMEOUT_DFA_STATES,
        NON_CACHEABLE_TEST_TIMEOUT_PATTERNS, PatternMatcher, classifier_dfa,
        non_cacheable_test_timeout_dfa,
    };

    fn oracle_mask(input: &[u8]) -> u64 {
        let lowered: Vec<u8> = input.iter().map(u8::to_ascii_lowercase).collect();
        CLASSIFIER_PATTERNS
            .iter()
            .enumerate()
            .fold(0u64, |mask, (index, pattern)| {
                let needle = pattern.as_bytes();
                if lowered.windows(needle.len()).any(|window| window == needle) {
                    mask | (1u64 << index)
                } else {
                    mask
                }
            })
    }

    fn matcher_mask_with_chunks(input: &[u8], chunks: &[usize]) -> u64 {
        let mut matcher = PatternMatcher::default();
        let mut offset = 0usize;
        for chunk in chunks {
            let end = offset.saturating_add(*chunk).min(input.len());
            matcher.feed(&input[offset..end]);
            offset = end;
        }
        matcher.feed(&input[offset..]);
        let matched = matcher.matched_patterns();
        CLASSIFIER_PATTERNS
            .iter()
            .enumerate()
            .fold(0u64, |mask, (index, pattern)| {
                if matched.contains(pattern) {
                    mask | (1u64 << index)
                } else {
                    mask
                }
            })
    }

    fn alternating_case(pattern: &str) -> Vec<u8> {
        pattern
            .bytes()
            .enumerate()
            .map(|(index, byte)| {
                if index % 2 == 0 {
                    byte.to_ascii_uppercase()
                } else {
                    byte.to_ascii_lowercase()
                }
            })
            .collect()
    }

    #[test]
    fn dfa_stays_within_the_fixed_dense_table_bounds() {
        let dfa = classifier_dfa();
        assert!(dfa.nodes.len() <= MAX_DFA_STATES);
        assert!(non_cacheable_test_timeout_dfa().nodes.len() <= MAX_TEST_TIMEOUT_DFA_STATES);
        assert_eq!(DFA_COLUMNS, 128);
        assert!(
            dfa.nodes
                .iter()
                .all(|node| node.transitions.len() == DFA_COLUMNS)
        );
    }

    #[test]
    fn every_pattern_and_mixed_case_pattern_matches_at_every_chunk_split() {
        for pattern in CLASSIFIER_PATTERNS {
            for bytes in [pattern.as_bytes().to_vec(), alternating_case(pattern)] {
                let expected = oracle_mask(&bytes);
                for split in 0..=bytes.len() {
                    assert_eq!(
                        matcher_mask_with_chunks(&bytes, &[split]),
                        expected,
                        "pattern {pattern:?} split at {split}"
                    );
                }
            }
        }
    }

    #[test]
    fn overlapping_patterns_and_ascii_folding_match_the_reference() {
        for input in [
            b"x: COMMAND NOT FOUND; command not found",
            b"prefix TARGET/DEBUG suffix",
            b"NO SPACE LEFT ON DEVICE and no space left on device",
        ] {
            let expected = oracle_mask(input);
            assert_eq!(matcher_mask_with_chunks(input, &[1, 2, 3, 5]), expected);
        }

        let nested = oracle_mask(b": command not found");
        let colon_command = CLASSIFIER_PATTERNS
            .iter()
            .position(|p| *p == ": command not found")
            .unwrap();
        let command = CLASSIFIER_PATTERNS
            .iter()
            .position(|p| *p == "command not found")
            .unwrap();
        assert_ne!(nested & (1u64 << colon_command), 0);
        assert_ne!(nested & (1u64 << command), 0);
    }

    #[test]
    fn timeout_text_is_tracked_separately_from_classifier_evidence() {
        for pattern in NON_CACHEABLE_TEST_TIMEOUT_PATTERNS {
            for bytes in [pattern.as_bytes().to_vec(), alternating_case(pattern)] {
                for split in 0..=bytes.len() {
                    let mut matcher = PatternMatcher::default();
                    matcher.feed(&bytes[..split]);
                    matcher.feed(&bytes[split..]);
                    assert!(matcher.saw_test_timeout_text(), "pattern {pattern:?}");
                    assert!(
                        matcher.matched_patterns().is_empty(),
                        "timeout text appeared as classifier evidence: {pattern:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn non_ascii_bytes_break_ascii_patterns_without_changing_following_matches() {
        for input in [
            b"permissi\xC3\xB3n denied".as_slice(),
            b"permission denied\xFF",
            b"\xC3permission denied",
        ] {
            assert_eq!(
                matcher_mask_with_chunks(input, &[1, 2, 1, 4]),
                oracle_mask(input)
            );
        }
        assert_eq!(
            matcher_mask_with_chunks(b"permission denied", &[2, 1]),
            oracle_mask(b"permission denied")
        );
    }

    #[test]
    fn deterministic_random_chunking_matches_full_lowered_byte_oracle() {
        let input = b"prefix COMMAND TIMED OUT, then \xC3 bytes, no space left on device; : COMMAND NOT FOUND; target/debug/.fingerprint";
        let expected = oracle_mask(input);
        for seed in 1u64..=256 {
            let mut random = seed;
            let mut offset = 0usize;
            let mut matcher = PatternMatcher::default();
            while offset < input.len() {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                let width = (random as usize % 19) + 1;
                let end = offset.saturating_add(width).min(input.len());
                matcher.feed(&input[offset..end]);
                offset = end;
            }
            assert_eq!(matcher.matched, expected, "chunk sequence seed {seed}");
        }
    }
}

#[cfg(unix)]
pub(crate) fn set_nonblocking<R: AsRawFd>(reader: &R) -> io::Result<()> {
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

#[cfg(not(unix))]
pub(crate) fn set_nonblocking<R>(_reader: &R) -> io::Result<()> {
    Err(unsupported_pipe_capture())
}

#[cfg(unix)]
pub(crate) fn drain_pipe<R: Read + AsRawFd + Send + 'static>(
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
                    first_output_ms
                        .compare_exchange(
                            -1,
                            started.elapsed().as_millis().min(i64::MAX as u128) as i64,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .unwrap_or_else(|recorded| recorded);
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

#[cfg(not(unix))]
pub(crate) fn drain_pipe<R: Read + Send + 'static>(
    _reader: R,
    _collector: Arc<Mutex<BoundedGateLog>>,
    _stop: Arc<AtomicBool>,
    _first_output_ms: Arc<AtomicI64>,
    drain_failed: Arc<AtomicBool>,
    drain_error: Arc<Mutex<Option<String>>>,
    _started: Instant,
) -> io::Result<()> {
    let error = unsupported_pipe_capture();
    record_drain_error(&drain_failed, &drain_error, &error);
    Err(error)
}

#[cfg(not(unix))]
fn unsupported_pipe_capture() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "bounded nonblocking gate pipe capture is unavailable on this platform",
    )
}

fn record_drain_error(failed: &AtomicBool, slot: &Mutex<Option<String>>, error: &io::Error) {
    if let Ok(mut message) = slot.lock()
        && message.is_none()
    {
        *message = Some(error.to_string());
    }
    failed.store(true, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_capture_caps_header_output_and_footer_together() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("agent.log");
        let file = File::create(&path).unwrap();
        let mut log = BoundedGateLog::from_file(file, AGENT_LOG_MAX_BYTES, "agent").unwrap();
        log.write_header(b"aethyme: agent output capture INCOMPLETE until finalized\n")
            .unwrap();
        log.sync_data().unwrap();
        assert!(
            std::fs::read(&path)
                .unwrap()
                .starts_with(b"aethyme: agent output capture INCOMPLETE")
        );
        let block = [b'x'; 64 * 1024];
        for _ in 0..300 {
            log.record_output(&block);
        }
        log.record_output(b"final agent tail");
        let summary = log.finish();
        log.sync_data().unwrap();
        assert_eq!(summary.output_bytes, 300 * 64 * 1024 + 16);
        assert!(!summary.incomplete);
        let saved = std::fs::read(&path).unwrap();
        assert!(saved.len() <= 16 * 1024 * 1024);
        assert!(saved.windows(16).any(|bytes| bytes == b"final agent tail"));
        let text = String::from_utf8_lossy(&saved);
        assert!(text.contains("agent output capture truncated"));
    }

    #[test]
    fn inherited_capture_refuses_existing_bytes_or_a_nonzero_cursor() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("existing.log");
        std::fs::write(&path, b"preserve").unwrap();
        let file = File::options().write(true).open(&path).unwrap();
        assert!(BoundedGateLog::from_file(file, AGENT_LOG_MAX_BYTES, "agent").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"preserve");
        let mut file = File::create(&path).unwrap();
        file.seek(io::SeekFrom::Start(32 * 1024 * 1024)).unwrap();
        assert!(BoundedGateLog::from_file(file, AGENT_LOG_MAX_BYTES, "agent").is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    }

    #[test]
    fn retains_bounded_prefix_tail_and_classifier_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gate.log");
        let mut log = BoundedGateLog::create(&path).unwrap();
        log.write_header(b"header\n").unwrap();
        let mut output = vec![b'a'; PREFIX_BUDGET_BYTES + 4096];
        let evidence_at = PREFIX_BUDGET_BYTES + 32;
        let evidence = b"no space left on device";
        output[evidence_at..evidence_at + evidence.len()].copy_from_slice(evidence);
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
    fn truncated_timeout_text_is_not_written_as_classifier_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gate.log");
        let mut log = BoundedGateLog::create(&path).unwrap();
        let mut output = vec![b'a'; PREFIX_BUDGET_BYTES + MAX_TAIL_BYTES + 128];
        let timeout_text = b"Test timed out in 5000ms";
        let timeout_at = PREFIX_BUDGET_BYTES + 64;
        output[timeout_at..timeout_at + timeout_text.len()].copy_from_slice(timeout_text);
        output.extend(std::iter::repeat_n(b'z', MAX_TAIL_BYTES));
        log.record_output(&output);
        log.finish();

        let saved = std::fs::read(&path).unwrap();
        let saved = String::from_utf8_lossy(&saved);
        assert!(saved.contains("test timeout text observed in complete gate stream"));
        assert!(!saved.contains("classifier evidence observed in complete gate stream: timed out"));
        assert!(has_test_timeout_text(&path));
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
