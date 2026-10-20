//! Byte accounting for agent-facing CLI output.
//!
//! Agent turns are charged per token, so the size of what a command prints is
//! a real cost — and until it is recorded, nobody can tell which commands are
//! expensive. This repository's `gc plan` reached ~319 KB of stdout, roughly
//! 80k tokens for a decision expressible in six lines, and it went unnoticed
//! because `command-metrics.jsonl` recorded duration and exit status but not
//! size. `gates affected` is the explicit read-only exception that records
//! phase timings by default so stalls can be localized; other inspection
//! commands remain telemetry-free unless output measurement is opted in.
//!
//! Counting happens at the single emission point [`out!`] rather than by
//! wrapping the process's stdout, so the number is exact for everything the
//! CLI prints and never includes output produced by a child process the
//! broker merely forwards.

use std::cell::Cell;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};

static EMITTED: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OutputSnapshot {
    emitted_bytes: u64,
    write_failures: u64,
}

thread_local! {
    static INVOCATION_OUTPUT: Cell<OutputSnapshot> = const {
        Cell::new(OutputSnapshot {
            emitted_bytes: 0,
            write_failures: 0,
        })
    };
}

pub(crate) fn snapshot() -> OutputSnapshot {
    INVOCATION_OUTPUT.with(Cell::get)
}

pub(crate) fn emitted_since(snapshot: OutputSnapshot) -> bool {
    emitted_bytes_since(snapshot) > 0
}

pub(crate) fn emitted_bytes_since(snapshot: OutputSnapshot) -> u64 {
    INVOCATION_OUTPUT.with(|state| {
        state
            .get()
            .emitted_bytes
            .saturating_sub(snapshot.emitted_bytes)
    })
}

pub(crate) fn failed_since(snapshot: OutputSnapshot) -> bool {
    INVOCATION_OUTPUT.with(|state| state.get().write_failures > snapshot.write_failures)
}

pub(crate) fn output_attempted_since(snapshot: OutputSnapshot) -> bool {
    emitted_since(snapshot) || failed_since(snapshot)
}

fn record_success(bytes: u64) {
    EMITTED.fetch_add(bytes, Ordering::Relaxed);
    INVOCATION_OUTPUT.with(|state| {
        let current = state.get();
        state.set(OutputSnapshot {
            emitted_bytes: current.emitted_bytes.saturating_add(bytes),
            write_failures: current.write_failures,
        });
    });
}

fn record_failure() {
    INVOCATION_OUTPUT.with(|state| {
        let current = state.get();
        state.set(OutputSnapshot {
            emitted_bytes: current.emitted_bytes,
            write_failures: current.write_failures.saturating_add(1),
        });
    });
}

/// Bytes this process has printed through out!.
pub(crate) fn emitted() -> u64 {
    EMITTED.load(Ordering::Relaxed)
}

pub(crate) fn exit_code_after_output(before: OutputSnapshot, code: u8) -> u8 {
    if code == crate::exit_status::SUCCESS && failed_since(before) {
        crate::exit_status::ENVIRONMENT
    } else {
        code
    }
}

fn write_line_to(writer: &mut impl Write, line: &str) -> io::Result<()> {
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

pub(crate) fn write_line_and_record(writer: &mut impl Write, line: &str) {
    let bytes = line.len() as u64 + 1;
    if write_line_to(writer, line).is_ok() {
        record_success(bytes);
    } else {
        record_failure();
    }
}

pub(crate) fn write_line(line: &str) {
    write_line_and_record(&mut io::stdout().lock(), line);
}

/// `println!`-like output; fully written lines count their bytes and newline.
macro_rules! out {
    () => {{
        $crate::cli_output::write_line("");
    }};
    ($($arg:tt)*) => {{
        let rendered = format!($($arg)*);
        $crate::cli_output::write_line(&rendered);
    }};
}

pub(crate) use out;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_line_records_complete_bytes_and_newlines() {
        let before = snapshot();
        let mut output = Vec::new();
        write_line_and_record(&mut output, "hello");
        assert_eq!(emitted_bytes_since(before), 6, "5 bytes plus the newline");
        write_line_and_record(&mut output, "a-12");
        assert_eq!(emitted_bytes_since(before), 6 + 5, "a-12 plus its newline");
        write_line_and_record(&mut output, "");
        assert_eq!(
            emitted_bytes_since(before),
            6 + 5 + 1,
            "a bare newline still costs a byte"
        );
        assert_eq!(output, b"hello\na-12\n\n");
    }

    #[test]
    fn a_failed_stdout_write_is_recorded_without_counting_bytes() {
        struct BrokenPipe;
        impl Write for BrokenPipe {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let before = snapshot();
        write_line_and_record(&mut BrokenPipe, "report");
        assert!(failed_since(before));
        assert!(!emitted_since(before));
        assert_eq!(emitted_bytes_since(before), 0);
        assert!(output_attempted_since(before));
        assert_eq!(
            exit_code_after_output(before, crate::exit_status::SUCCESS),
            crate::exit_status::ENVIRONMENT
        );
        assert_eq!(
            exit_code_after_output(before, crate::exit_status::REFUSED),
            crate::exit_status::REFUSED
        );
    }

    #[test]
    fn a_partial_stdout_write_is_recorded_as_an_output_attempt() {
        struct PrefixThenBrokenPipe {
            bytes: Vec<u8>,
            first_write: bool,
        }

        impl Write for PrefixThenBrokenPipe {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.first_write {
                    self.first_write = false;
                    let count = bytes.len().min(4);
                    self.bytes.extend_from_slice(&bytes[..count]);
                    Ok(count)
                } else {
                    Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
                }
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let before = snapshot();
        let mut writer = PrefixThenBrokenPipe {
            bytes: Vec::new(),
            first_write: true,
        };
        write_line_and_record(&mut writer, "report");

        assert_eq!(writer.bytes, b"repo");
        assert!(failed_since(before));
        assert!(output_attempted_since(before));
        assert!(!emitted_since(before));
        assert_eq!(emitted_bytes_since(before), 0);
    }
}
