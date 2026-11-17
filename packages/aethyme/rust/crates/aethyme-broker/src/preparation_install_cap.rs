//! A host-wide cap on concurrent preparation installs.
//!
//! Every repository's `submit prepare` runs its install steps holding one
//! unit of a single host capacity pool in the host-resource database, so a
//! host never runs more installs at once than it can afford, whatever the
//! number of repositories or sessions. Waiters are served in arrival order by
//! the coordinator's durable wait queue, and admission also follows the
//! host's memory pressure.
//!
//! The limit must be the same for every holder of the pool (the coordinator
//! refuses a request whose limit differs from an active holder's), so it is a
//! host property: `[installs] max_concurrent` in `<host state>/preparation.toml`,
//! or a default derived from the CPU count and physical memory. Free memory,
//! which changes from one request to the next, is followed through the
//! pool's memory-pressure admission instead of the limit.
//!
//! The slot is acquired after the repository's shared-cache lease (a holder of
//! the slot never waits for a cache lease, so the two cannot deadlock) and is
//! held only while install steps run.

use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::resources::CapacityPressure;
use crate::{
    HostResourceCoordinator, HostResourceError, HostResourceGrant, HostResourceKind,
    HostResourceRequest, HostResourceRequirement,
};

/// The host capacity pool every preparation install draws one unit from.
pub(crate) const INSTALL_POOL: &str = "aethyme-prepare-installs";
/// The host-state file holding `[installs] max_concurrent`.
pub(crate) const INSTALL_CAP_FILE: &str = "preparation.toml";
/// The most installs any default or configuration admits at once.
pub(crate) const INSTALL_CAP_MAX: u32 = 64;
/// Lease lifetime; the preparation heartbeat renews it while steps run.
pub(crate) const INSTALL_SLOT_TTL_SECONDS: u64 = 30;
/// Physical memory budgeted per concurrent install by the default.
const DEFAULT_BYTES_PER_INSTALL: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallCapFile {
    #[serde(default)]
    installs: InstallCapSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallCapSection {
    max_concurrent: Option<u32>,
}

/// Where the host's install limit came from.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstallCapSource {
    /// `[installs] max_concurrent` in `<host state>/preparation.toml`.
    Configured,
    /// Derived from the CPU count and physical memory.
    Default,
}

/// The default limit: half the CPUs, and one install per 4 GiB of physical
/// memory, whichever is smaller, at least 1 and at most [`INSTALL_CAP_MAX`].
pub(crate) fn default_install_limit(cpus: usize, memory_bytes: Option<u64>) -> u32 {
    let by_cpu = u64::try_from(cpus / 2).unwrap_or(u64::MAX).max(1);
    let by_memory = memory_bytes
        .map(|bytes| (bytes / DEFAULT_BYTES_PER_INSTALL).max(1))
        .unwrap_or(by_cpu);
    u32::try_from(by_cpu.min(by_memory))
        .unwrap_or(INSTALL_CAP_MAX)
        .clamp(1, INSTALL_CAP_MAX)
}

/// The host's install limit and where it came from. An unreadable or
/// invalid file keeps the default and says why.
pub(crate) fn install_limit(host_state: Option<&Path>) -> (u32, InstallCapSource, Option<String>) {
    let default = || {
        default_install_limit(
            std::thread::available_parallelism().map_or(1, usize::from),
            crate::environments::metrics::host_memory_bytes(),
        )
    };
    let Some(host_state) = host_state else {
        return (default(), InstallCapSource::Default, None);
    };
    let path = host_state.join(INSTALL_CAP_FILE);
    match std::fs::read_to_string(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (default(), InstallCapSource::Default, None)
        }
        Err(error) => (
            default(),
            InstallCapSource::Default,
            Some(format!("{INSTALL_CAP_FILE}: {error}")),
        ),
        Ok(text) => match parse_install_limit(&text) {
            Ok(Some(limit)) => (limit, InstallCapSource::Configured, None),
            Ok(None) => (default(), InstallCapSource::Default, None),
            Err(reason) => (
                default(),
                InstallCapSource::Default,
                Some(format!("{INSTALL_CAP_FILE}: {reason}")),
            ),
        },
    }
}

/// Parse `[installs] max_concurrent`; `Ok(None)` when it is not set.
pub(crate) fn parse_install_limit(text: &str) -> Result<Option<u32>, String> {
    let file = toml::from_str::<InstallCapFile>(text).map_err(|error| error.to_string())?;
    match file.installs.max_concurrent {
        None => Ok(None),
        Some(limit) if (1..=INSTALL_CAP_MAX).contains(&limit) => Ok(Some(limit)),
        Some(limit) => Err(format!(
            "installs.max_concurrent must be 1..={INSTALL_CAP_MAX}, not {limit}"
        )),
    }
}

/// What the install slot did for one preparation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstallSlotReport {
    pub limit: u32,
    pub limit_source: InstallCapSource,
    /// Time spent waiting for the slot.
    pub wait_ms: u64,
    /// Install leases active when the slot was granted, this one excluded.
    pub other_holders: usize,
    /// Why the configured limit was not used, when the file was unreadable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration_warning: Option<String>,
}

/// One held install slot. Dropping it releases the lease.
pub(crate) struct InstallSlot {
    coordinator: HostResourceCoordinator,
    grant: Option<HostResourceGrant>,
}

impl InstallSlot {
    /// Renew the lease; the preparation heartbeat calls this while steps run.
    pub(crate) fn renew(&mut self) -> Result<(), HostResourceError> {
        if let Some(grant) = self.grant.as_mut() {
            grant.lease = self.coordinator.renew(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
                INSTALL_SLOT_TTL_SECONDS,
            )?;
        }
        Ok(())
    }

    /// Release the lease now.
    pub(crate) fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if let Some(grant) = self.grant.take() {
            crate::warn_unrecorded(
                "release the preparation install slot",
                self.coordinator.release(
                    &grant.lease.lease_id,
                    grant.lease.generation,
                    &grant.ownership_token,
                ),
            );
        }
    }
}

impl Drop for InstallSlot {
    fn drop(&mut self) {
        self.release_inner();
    }
}

/// Why no install slot was granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstallSlotRefusal {
    pub(crate) limit: u32,
    pub(crate) holders: usize,
    pub(crate) waited_ms: u64,
    pub(crate) timed_out: bool,
    pub(crate) reason: String,
}

impl InstallSlotRefusal {
    pub(crate) fn next_action(&self, session_id: i64) -> String {
        format!(
            "retry `aethyme broker submit prepare --session {session_id} --wait <longer>` once one of the {} running install(s) finishes, or raise `[installs] max_concurrent` (now {}) in <host state>/{INSTALL_CAP_FILE}; `aethyme broker advanced resources list` shows the holders",
            self.holders, self.limit
        )
    }
}

/// The request for one install slot.
pub(crate) fn install_slot_request(
    repository: &str,
    session_id: i64,
    worktree: &Path,
    limit: u32,
) -> HostResourceRequest {
    use sha2::{Digest, Sha256};
    static NEXT_ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let attempt = NEXT_ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let run_id = format!("prepare-install-s{session_id}");
    HostResourceRequest {
        schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
        request_id: format!("{run_id}-{}-{now}-{attempt}", std::process::id()),
        repository: repository.to_owned(),
        worktree_fingerprint: format!(
            "{:x}",
            Sha256::digest(worktree.to_string_lossy().as_bytes())
        ),
        run_id,
        ttl_seconds: INSTALL_SLOT_TTL_SECONDS,
        holder_pid: Some(std::process::id()),
        resources: vec![HostResourceRequirement {
            key: "preparation_install".into(),
            resource: HostResourceKind::Capacity {
                pool: INSTALL_POOL.into(),
                units: 1,
                limit,
                pressure: Some(CapacityPressure::Memory),
            },
        }],
    }
}

/// Active install leases other than `except`.
pub(crate) fn install_holders(
    coordinator: &HostResourceCoordinator,
    except: Option<&str>,
) -> usize {
    coordinator
        .list(false)
        .map(|leases| {
            leases
                .iter()
                .filter(|lease| lease.state == crate::HostLeaseState::Active)
                .filter(|lease| Some(lease.lease_id.as_str()) != except)
                .filter(|lease| {
                    lease.allocations.iter().any(|allocation| {
                        allocation.kind == "capacity" && allocation.value == INSTALL_POOL
                    })
                })
                .count()
        })
        .unwrap_or(0)
}

/// Wait in arrival order, up to `wait`, for one install slot.
pub(crate) fn acquire_install_slot(
    resource_db_path: &Path,
    host_state: Option<&Path>,
    repository: &str,
    session_id: i64,
    worktree: &Path,
    wait: Duration,
) -> Result<(InstallSlot, InstallSlotReport), InstallSlotRefusal> {
    let (limit, limit_source, configuration_warning) = install_limit(host_state);
    if let Some(warning) = &configuration_warning {
        eprintln!("warning: aethyme keeps the default install limit {limit}: {warning}");
    }
    let started = Instant::now();
    let refusal = |reason: String, timed_out: bool, holders: usize| InstallSlotRefusal {
        limit,
        holders,
        waited_ms: elapsed_ms(started),
        timed_out,
        reason,
    };
    let mut coordinator = HostResourceCoordinator::open(resource_db_path).map_err(|error| {
        refusal(
            format!("cannot open the host resource database: {error}"),
            false,
            0,
        )
    })?;
    let request = install_slot_request(repository, session_id, worktree, limit);
    let mut announced = false;
    let result = coordinator.acquire_with_wait(&request, wait, |message| {
        if !announced {
            announced = true;
            eprintln!("preparation: waiting for one of {limit} host install slot(s): {message}");
        }
    });
    let waited = started.elapsed();
    crate::phase_telemetry::add("install_slot_wait", waited);
    match result {
        Ok(grant) => {
            crate::phase_telemetry::add("install_slot_admitted", Duration::ZERO);
            let other_holders = install_holders(&coordinator, Some(&grant.lease.lease_id));
            Ok((
                InstallSlot {
                    coordinator,
                    grant: Some(grant),
                },
                InstallSlotReport {
                    limit,
                    limit_source,
                    wait_ms: elapsed_ms(started),
                    other_holders,
                    configuration_warning,
                },
            ))
        }
        Err(error) => {
            let holders = install_holders(&coordinator, None);
            let timed_out = matches!(&error, HostResourceError::Conflict { conflicts, .. }
                if conflicts.iter().all(|conflict| conflict.code == "resource_contention"));
            crate::phase_telemetry::add(
                if timed_out {
                    "install_slot_timed_out"
                } else {
                    "install_slot_refused"
                },
                Duration::ZERO,
            );
            Err(refusal(error.to_string(), timed_out, holders))
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_limit_follows_cpus_and_memory() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(default_install_limit(12, Some(64 * GIB)), 6);
        assert_eq!(default_install_limit(12, Some(16 * GIB)), 4);
        assert_eq!(default_install_limit(1, Some(64 * GIB)), 1);
        assert_eq!(default_install_limit(8, Some(GIB)), 1);
        assert_eq!(default_install_limit(8, None), 4);
        assert_eq!(
            default_install_limit(1024, Some(1024 * GIB)),
            INSTALL_CAP_MAX
        );
    }

    #[test]
    fn the_configured_limit_is_validated() {
        assert_eq!(parse_install_limit(""), Ok(None));
        assert_eq!(parse_install_limit("[installs]\n"), Ok(None));
        assert_eq!(
            parse_install_limit("[installs]\nmax_concurrent = 3\n"),
            Ok(Some(3))
        );
        assert!(parse_install_limit("[installs]\nmax_concurrent = 0\n").is_err());
        assert!(parse_install_limit("[installs]\nmax_concurrent = 65\n").is_err());
        assert!(parse_install_limit("[installs]\nunknown = 1\n").is_err());
    }

    #[test]
    fn a_configured_file_wins_and_a_broken_one_keeps_the_default() {
        let host = tempfile::tempdir().unwrap();
        let (_, source, warning) = install_limit(Some(host.path()));
        assert_eq!((source, warning), (InstallCapSource::Default, None));
        std::fs::write(
            host.path().join(INSTALL_CAP_FILE),
            "[installs]\nmax_concurrent = 2\n",
        )
        .unwrap();
        assert_eq!(
            install_limit(Some(host.path())),
            (2, InstallCapSource::Configured, None)
        );
        std::fs::write(
            host.path().join(INSTALL_CAP_FILE),
            "[installs]\nmax_concurrent = 0\n",
        )
        .unwrap();
        let (_, source, warning) = install_limit(Some(host.path()));
        assert_eq!(source, InstallCapSource::Default);
        assert!(warning.unwrap().contains("max_concurrent"));
    }

    #[test]
    fn slots_are_capped_host_wide_and_a_refusal_names_holders_and_a_next_action() {
        let host = tempfile::tempdir().unwrap();
        std::fs::write(
            host.path().join(INSTALL_CAP_FILE),
            "[installs]\nmax_concurrent = 1\n",
        )
        .unwrap();
        let db = host.path().join("host-resources.sqlite3");
        let worktree = host.path().join("checkout");
        let (slot, report) = acquire_install_slot(
            &db,
            Some(host.path()),
            "repository-a",
            1,
            &worktree,
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(report.limit, 1);
        assert_eq!(report.limit_source, InstallCapSource::Configured);
        assert_eq!(report.other_holders, 0);
        // Another repository is capped by the same host pool.
        let refusal = match acquire_install_slot(
            &db,
            Some(host.path()),
            "repository-b",
            2,
            &worktree,
            Duration::ZERO,
        ) {
            Ok(_) => panic!("a second install must wait for the only slot"),
            Err(refusal) => refusal,
        };
        assert_eq!(refusal.holders, 1);
        assert_eq!(refusal.limit, 1);
        assert!(refusal.timed_out, "{refusal:?}");
        let next = refusal.next_action(2);
        assert!(
            next.contains("--session 2") && next.contains("max_concurrent"),
            "{next}"
        );
        slot.release();
        let (_slot, report) = acquire_install_slot(
            &db,
            Some(host.path()),
            "repository-b",
            2,
            &worktree,
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(report.other_holders, 0);
    }
}
