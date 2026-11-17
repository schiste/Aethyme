//! Bounded bytes per second for unattended archiving.
//!
//! The lane reads at a configured rate with a floor, so it never competes
//! with the work a person or an agent is waiting on and still always makes
//! progress. When the archive volume is below its free-space floor the space
//! is needed now and the lane runs unpaced.

use std::time::{Duration, Instant};

use super::config::MIN_BYTES_PER_SECOND;

/// The rate for one file: `None` (unpaced) below the free-space floor,
/// otherwise the configured rate clamped to the floor rate.
pub(crate) fn choose_rate(
    configured_bytes_per_second: u64,
    available_bytes: Option<u64>,
    free_space_floor_bytes: u64,
) -> Option<u64> {
    match available_bytes {
        Some(available) if available < free_space_floor_bytes => None,
        _ => Some(configured_bytes_per_second.max(MIN_BYTES_PER_SECOND)),
    }
}

/// Room an archive needs on its volume before the original is removed: a
/// third of the original's size (zstd -3 does 4-5x on transcripts) plus a
/// fixed margin, so archiving never fills the disk to make space.
pub(crate) const SPACE_MARGIN_BYTES: u64 = 256 * 1024 * 1024;

/// Whether `bytes` of transcript may be archived with `available` bytes
/// free on the archive volume.
pub(crate) fn fits(available: u64, bytes: u64) -> bool {
    available >= (bytes / 3).saturating_add(SPACE_MARGIN_BYTES)
}

/// Sleeps as needed so the bytes it is told about never run ahead of the rate.
#[derive(Debug)]
pub(crate) struct Pacer {
    rate: Option<u64>,
    window_started: Instant,
    window_bytes: u64,
    slept: Duration,
}

impl Pacer {
    pub fn new(rate: Option<u64>) -> Self {
        Self {
            rate,
            window_started: Instant::now(),
            window_bytes: 0,
            slept: Duration::ZERO,
        }
    }

    /// Change the rate; the accounting window restarts so a change never
    /// owes or forgives earlier bytes.
    pub fn set_rate(&mut self, rate: Option<u64>) {
        if rate != self.rate {
            self.rate = rate;
            self.window_started = Instant::now();
            self.window_bytes = 0;
        }
    }

    pub fn rate(&self) -> Option<u64> {
        self.rate
    }

    /// Account for `bytes` just read, sleeping when ahead of the rate.
    pub fn consume(&mut self, bytes: u64) {
        let Some(rate) = self.rate.filter(|rate| *rate > 0) else {
            return;
        };
        self.window_bytes = self.window_bytes.saturating_add(bytes);
        let due = Duration::from_secs_f64(self.window_bytes as f64 / rate as f64);
        let elapsed = self.window_started.elapsed();
        if due > elapsed {
            let wait = due - elapsed;
            std::thread::sleep(wait);
            self.slept += wait;
        }
    }

    /// Total time spent sleeping.
    pub fn slept(&self) -> Duration {
        self.slept
    }
}
