//! Byte accounting for agent-facing CLI output.
//!
//! Agent turns are charged per token, so the size of what a command prints is
//! a real cost — and until it is recorded, nobody can tell which commands are
//! expensive. This repository's `gc plan` reached ~319 KB of stdout, roughly
//! 80k tokens for a decision expressible in six lines, and it went unnoticed
//! because `command-metrics.jsonl` recorded duration and exit status but not
//! size.
//!
//! Counting happens at the single emission point [`out!`] rather than by
//! wrapping the process's stdout, so the number is exact for everything the
//! CLI prints and never includes output produced by a child process the
//! broker merely forwards.

use std::sync::atomic::{AtomicU64, Ordering};

static EMITTED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn count(bytes: u64) {
    EMITTED.fetch_add(bytes, Ordering::Relaxed);
}

/// Bytes this process has printed through [`out!`].
pub(crate) fn emitted() -> u64 {
    EMITTED.load(Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn reset() {
    EMITTED.store(0, Ordering::Relaxed);
}

/// `println!` that records what it printed.
///
/// Identical in output to `println!`; it formats first so the byte count is
/// exact, including the trailing newline.
macro_rules! out {
    () => {{
        $crate::cli_output::count(1);
        println!();
    }};
    ($($arg:tt)*) => {{
        let rendered = format!($($arg)*);
        $crate::cli_output::count(rendered.len() as u64 + 1);
        println!("{rendered}");
    }};
}

pub(crate) use out;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emitted_bytes_match_what_was_printed() {
        reset();
        out!("hello");
        assert_eq!(emitted(), 6, "5 bytes plus the newline");
        out!("{}-{}", "a", 12);
        assert_eq!(emitted(), 6 + 5, "`a-12` plus its newline");
        out!();
        assert_eq!(emitted(), 6 + 5 + 1, "a bare newline still costs a byte");
    }
}
