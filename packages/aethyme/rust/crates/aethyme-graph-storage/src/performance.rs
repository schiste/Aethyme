//! Stable, content-free performance observations for graph lifecycle commands.

use serde::{Deserialize, Serialize};

/// Work observed for one named lifecycle phase.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphPhaseObservation {
    /// Monotonic wall time spent in the phase. This is evidence, not plan authority.
    pub elapsed_us: u128,
    /// Bytes actually loaded by the phase where the boundary is observable.
    pub bytes_read: u64,
    /// Bytes actually persisted or staged by the phase where observable.
    pub bytes_written: u64,
}

/// Logical graph size observed without including node contents or source paths.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphEntityCounts {
    pub files: Option<usize>,
    pub nodes: Option<usize>,
    pub edges: Option<usize>,
}

/// Phase-level observability shared by refresh, materialization, and Explore.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphLifecycleObservability {
    pub repository_discovery: GraphPhaseObservation,
    pub source_snapshot: GraphPhaseObservation,
    pub policy_loading: GraphPhaseObservation,
    pub fragment_validation: GraphPhaseObservation,
    pub source_indexing: GraphPhaseObservation,
    pub fragment_serialization: GraphPhaseObservation,
    pub graph_linking: GraphPhaseObservation,
    pub fragment_application: GraphPhaseObservation,
    pub redb_materialization: GraphPhaseObservation,
    pub totals: GraphPhaseObservation,
    pub counts: GraphEntityCounts,
    /// Process high-water RSS where the host exposes it. It is process-wide,
    /// so consumers should compare separate command invocations.
    pub peak_memory_bytes: Option<u64>,
}

impl GraphLifecycleObservability {
    pub fn finish(&mut self, elapsed_us: u128) {
        self.totals.elapsed_us = elapsed_us;
        self.totals.bytes_read = self.repository_discovery.bytes_read
            + self.source_snapshot.bytes_read
            + self.policy_loading.bytes_read
            + self.fragment_validation.bytes_read
            + self.source_indexing.bytes_read
            + self.fragment_serialization.bytes_read
            + self.graph_linking.bytes_read
            + self.fragment_application.bytes_read
            + self.redb_materialization.bytes_read;
        self.totals.bytes_written = self.repository_discovery.bytes_written
            + self.source_snapshot.bytes_written
            + self.policy_loading.bytes_written
            + self.fragment_validation.bytes_written
            + self.source_indexing.bytes_written
            + self.fragment_serialization.bytes_written
            + self.graph_linking.bytes_written
            + self.fragment_application.bytes_written
            + self.redb_materialization.bytes_written;
        self.peak_memory_bytes = peak_memory_bytes();
    }
}

/// Return the process high-water resident set size when supported.
pub fn peak_memory_bytes() -> Option<u64> {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage initializes the provided rusage on success.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: the successful call above initialized the value.
        let maximum = unsafe { usage.assume_init() }.ru_maxrss;
        if maximum < 0 {
            return None;
        }
        #[cfg(target_os = "macos")]
        return Some(maximum as u64);
        #[cfg(not(target_os = "macos"))]
        return (maximum as u64).checked_mul(1024);
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_are_derived_from_explicit_phase_boundaries() {
        let mut observation = GraphLifecycleObservability::default();
        observation.policy_loading.bytes_read = 12;
        observation.fragment_validation.bytes_read = 30;
        observation.fragment_serialization.bytes_written = 40;
        observation.redb_materialization.bytes_written = 50;
        observation.finish(99);
        assert_eq!(observation.totals.elapsed_us, 99);
        assert_eq!(observation.totals.bytes_read, 42);
        assert_eq!(observation.totals.bytes_written, 90);
    }
}
