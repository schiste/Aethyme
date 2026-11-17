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
//! refuses a request whose limit differs from an active holder's). So a
//! request always adopts the limit of the pool's active holders; with none
//! active it uses `[installs] max_concurrent` in `<host state>/preparation.toml`
//! or a default derived from the CPU count and physical memory. The default
//! is persisted in host state and reused, recomputed only while no install
//! holds the pool and the host facts changed, because two callers (one
//! sandboxed, one not) can see different CPU counts or no memory size. Free
//! memory, which changes from one request to the next, is followed through
//! the pool's memory-pressure admission instead of the limit.
//!
//! A preparation started from inside a preparation step finds its parent's
//! slot in [`PARENT_SLOT_ENV`] and runs under it instead of waiting for a
//! second slot, which at limit 1 would wait on itself.
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
/// Set in every preparation step's environment to the lease id of the slot
/// the step runs under, so a nested preparation reuses it.
pub(crate) const PARENT_SLOT_ENV: &str = "AETHYME_PREPARE_INSTALL_SLOT";
/// The host-state file persisting the chosen default limit.
pub(crate) const DEFAULT_LIMIT_FILE: &str = "preparation-install-limit.json";
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
    /// Derived from the CPU count and physical memory (persisted).
    Default,
    /// The limit of the installs already holding the pool.
    ActivePool,
    /// No slot was taken: a parent preparation's slot covers this one.
    ParentSlot,
}

/// The host facts a default limit was derived from.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct HostFacts {
    pub(crate) cpus: usize,
    pub(crate) memory_bytes: Option<u64>,
}

impl HostFacts {
    pub(crate) fn observe() -> Self {
        Self {
            cpus: std::thread::available_parallelism().map_or(1, usize::from),
            memory_bytes: crate::environments::metrics::host_memory_bytes(),
        }
    }
}

/// A persisted default limit and the facts it came from.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct PersistedDefault {
    pub(crate) limit: u32,
    pub(crate) facts: HostFacts,
}

/// The limit one request uses, and what to persist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LimitChoice {
    pub(crate) limit: u32,
    pub(crate) source: InstallCapSource,
    pub(crate) warning: Option<String>,
    pub(crate) persist: Option<PersistedDefault>,
}

/// Choose the limit for one request. Active holders' limit always wins (a
/// differing configured value applies once they finish, and says so); with
/// none active a configured value wins; otherwise the persisted default is
/// reused unless the host facts changed and are fully known now.
pub(crate) fn choose_limit(
    configured: Option<u32>,
    active: Option<u32>,
    persisted: Option<PersistedDefault>,
    facts: HostFacts,
) -> LimitChoice {
    if let Some(active) = active {
        let warning = configured.filter(|configured| *configured != active).map(|configured| {
            format!(
                "[installs] max_concurrent = {configured} applies once the installs holding the pool under limit {active} finish; this preparation uses {active}"
            )
        });
        return LimitChoice {
            limit: active,
            source: InstallCapSource::ActivePool,
            warning,
            persist: None,
        };
    }
    if let Some(configured) = configured {
        return LimitChoice {
            limit: configured,
            source: InstallCapSource::Configured,
            warning: None,
            persist: None,
        };
    }
    let reuse = persisted.filter(|persisted| {
        // A sandboxed caller may see no memory size or fewer CPUs: never let
        // partial facts replace a default derived from complete ones.
        persisted.facts == facts || facts.memory_bytes.is_none()
    });
    match reuse {
        Some(persisted) => LimitChoice {
            limit: persisted.limit,
            source: InstallCapSource::Default,
            warning: None,
            persist: None,
        },
        None => {
            let limit = default_install_limit(facts.cpus, facts.memory_bytes);
            LimitChoice {
                limit,
                source: InstallCapSource::Default,
                warning: None,
                persist: Some(PersistedDefault { limit, facts }),
            }
        }
    }
}

fn read_persisted_default(host_state: Option<&Path>) -> Option<PersistedDefault> {
    let bytes = std::fs::read(host_state?.join(DEFAULT_LIMIT_FILE)).ok()?;
    serde_json::from_slice::<PersistedDefault>(&bytes)
        .ok()
        .filter(|persisted| (1..=INSTALL_CAP_MAX).contains(&persisted.limit))
}

fn write_persisted_default(host_state: &Path, persisted: &PersistedDefault) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(persisted).map_err(std::io::Error::other)?;
    let mut file = tempfile::NamedTempFile::new_in(host_state)?;
    std::io::Write::write_all(&mut file, &bytes)?;
    file.persist(host_state.join(DEFAULT_LIMIT_FILE))
        .map_err(|error| error.error)?;
    Ok(())
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

/// The configured limit, if any. An unreadable or invalid file counts as
/// unset and says why.
pub(crate) fn configured_limit(host_state: Option<&Path>) -> (Option<u32>, Option<String>) {
    let Some(host_state) = host_state else {
        return (None, None);
    };
    match std::fs::read_to_string(host_state.join(INSTALL_CAP_FILE)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(error) => (None, Some(format!("{INSTALL_CAP_FILE}: {error}"))),
        Ok(text) => match parse_install_limit(&text) {
            Ok(limit) => (limit, None),
            Err(reason) => (None, Some(format!("{INSTALL_CAP_FILE}: {reason}"))),
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
    /// Why the configured limit was not used: an unreadable file, or active
    /// installs holding the pool under another limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration_warning: Option<String>,
    /// Slot renewals that failed while steps ran; the install continued.
    #[serde(skip_serializing_if = "is_zero")]
    pub renewal_failures: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// One held install slot. Dropping it releases the lease.
pub(crate) struct InstallSlot {
    /// Absent for a slot borrowed from a parent preparation.
    coordinator: Option<HostResourceCoordinator>,
    grant: Option<HostResourceGrant>,
}

impl InstallSlot {
    /// Renew the lease; the preparation heartbeat calls this while steps run.
    /// A failure is reported, counted and otherwise ignored: an install that
    /// already started is never stopped for its slot (admitting one install
    /// too many later is the accepted cost). Returns whether it renewed.
    pub(crate) fn renew_best_effort(&mut self) -> bool {
        let (Some(coordinator), Some(grant)) = (self.coordinator.as_mut(), self.grant.as_mut())
        else {
            return true;
        };
        match coordinator.renew(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
            INSTALL_SLOT_TTL_SECONDS,
        ) {
            Ok(lease) => {
                grant.lease = lease;
                true
            }
            Err(error) => {
                crate::phase_telemetry::add("install_slot_renewal_failed", Duration::ZERO);
                eprintln!(
                    "warning: aethyme could not renew the preparation install slot; the install continues: {error}"
                );
                false
            }
        }
    }

    /// The lease id nested preparations reuse, when this slot holds one.
    pub(crate) fn lease_id(&self) -> Option<&str> {
        self.grant
            .as_ref()
            .map(|grant| grant.lease.lease_id.as_str())
    }

    /// Release the lease now.
    pub(crate) fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if let (Some(coordinator), Some(grant)) = (self.coordinator.as_mut(), self.grant.take()) {
            crate::warn_unrecorded(
                "release the preparation install slot",
                coordinator.release(
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
        if self.timed_out {
            format!(
                "retry `aethyme broker submit prepare --session {session_id} --wait <longer>` once one of the {} running install(s) finishes, or raise `[installs] max_concurrent` (now {}) in <host state>/{INSTALL_CAP_FILE}; `aethyme broker advanced resources list` shows the holders",
                self.holders, self.limit
            )
        } else {
            format!(
                "the install pool refused this request without a wait that could help ({}); inspect `aethyme broker advanced resources list` and reconcile any stranded install lease, then retry `aethyme broker submit prepare --session {session_id}`",
                self.reason
            )
        }
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

/// The limit of the installs holding the pool now, if any hold it.
pub(crate) fn active_pool_limit(coordinator: &HostResourceCoordinator) -> Option<u32> {
    coordinator.list(false).ok()?.iter().find_map(|lease| {
        (lease.state == crate::HostLeaseState::Active)
            .then(|| {
                lease.allocations.iter().find_map(|allocation| {
                    (allocation.kind == "capacity" && allocation.value == INSTALL_POOL)
                        .then_some(allocation.capacity_limit)
                        .flatten()
                })
            })
            .flatten()
    })
}

/// Whether `lease_id` is an active install lease.
fn active_install_lease(coordinator: &HostResourceCoordinator, lease_id: &str) -> bool {
    coordinator.list(false).is_ok_and(|leases| {
        leases.iter().any(|lease| {
            lease.lease_id == lease_id
                && lease.state == crate::HostLeaseState::Active
                && lease.allocations.iter().any(|allocation| {
                    allocation.kind == "capacity" && allocation.value == INSTALL_POOL
                })
        })
    })
}

/// Wait in arrival order, up to `wait`, for one install slot. `parent_slot`
/// is the inherited [`PARENT_SLOT_ENV`]: while that lease is active, this
/// preparation runs under it and takes no slot.
pub(crate) fn acquire_install_slot(
    resource_db_path: &Path,
    host_state: Option<&Path>,
    repository: &str,
    session_id: i64,
    worktree: &Path,
    wait: Duration,
    parent_slot: Option<&str>,
) -> Result<(InstallSlot, InstallSlotReport), InstallSlotRefusal> {
    let started = Instant::now();
    let refusal =
        |reason: String, timed_out: bool, holders: usize, limit: u32| InstallSlotRefusal {
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
            0,
        )
    })?;
    if let Some(parent) = parent_slot.filter(|parent| active_install_lease(&coordinator, parent)) {
        crate::phase_telemetry::add("install_slot_parent_reused", Duration::ZERO);
        let limit = active_pool_limit(&coordinator).unwrap_or(0);
        return Ok((
            InstallSlot {
                coordinator: None,
                grant: None,
            },
            InstallSlotReport {
                limit,
                limit_source: InstallCapSource::ParentSlot,
                wait_ms: 0,
                other_holders: install_holders(&coordinator, Some(parent)),
                configuration_warning: None,
                renewal_failures: 0,
            },
        ));
    }
    let (configured, file_warning) = configured_limit(host_state);
    let facts = HostFacts::observe();
    let persisted = read_persisted_default(host_state);
    // Another process may start or stop holding the pool between choosing a
    // limit and asking for it; a mismatch is then re-planned once.
    let mut attempts = 0;
    loop {
        attempts += 1;
        let choice = choose_limit(
            configured,
            active_pool_limit(&coordinator),
            persisted,
            facts,
        );
        if let (Some(persist), Some(host_state)) = (choice.persist, host_state) {
            crate::warn_unrecorded(
                "persist the default preparation install limit",
                write_persisted_default(host_state, &persist),
            );
        }
        let warning = file_warning.clone().or(choice.warning.clone());
        if let Some(warning) = &warning {
            eprintln!("warning: aethyme install limit {}: {warning}", choice.limit);
        }
        let request = install_slot_request(repository, session_id, worktree, choice.limit);
        let mut announced = false;
        let limit = choice.limit;
        let result = coordinator.acquire_with_wait(
            &request,
            wait.saturating_sub(started.elapsed()),
            |message| {
                if !announced {
                    announced = true;
                    eprintln!(
                        "preparation: waiting for one of {limit} host install slot(s): {message}"
                    );
                }
            },
        );
        match result {
            Ok(grant) => {
                crate::phase_telemetry::add("install_slot_wait", started.elapsed());
                crate::phase_telemetry::add("install_slot_admitted", Duration::ZERO);
                let other_holders = install_holders(&coordinator, Some(&grant.lease.lease_id));
                return Ok((
                    InstallSlot {
                        coordinator: Some(coordinator),
                        grant: Some(grant),
                    },
                    InstallSlotReport {
                        limit,
                        limit_source: choice.source,
                        wait_ms: elapsed_ms(started),
                        other_holders,
                        configuration_warning: warning,
                        renewal_failures: 0,
                    },
                ));
            }
            Err(HostResourceError::Conflict { conflicts, .. })
                if attempts < 2
                    && conflicts
                        .iter()
                        .any(|conflict| conflict.code == "capacity_policy_mismatch") =>
            {
                crate::phase_telemetry::add("install_slot_limit_replanned", Duration::ZERO);
            }
            Err(error) => {
                crate::phase_telemetry::add("install_slot_wait", started.elapsed());
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
                return Err(refusal(error.to_string(), timed_out, holders, limit));
            }
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
    fn a_configured_file_is_read_and_a_broken_one_counts_as_unset() {
        let host = tempfile::tempdir().unwrap();
        assert_eq!(configured_limit(Some(host.path())), (None, None));
        std::fs::write(
            host.path().join(INSTALL_CAP_FILE),
            "[installs]\nmax_concurrent = 2\n",
        )
        .unwrap();
        assert_eq!(configured_limit(Some(host.path())), (Some(2), None));
        std::fs::write(
            host.path().join(INSTALL_CAP_FILE),
            "[installs]\nmax_concurrent = 0\n",
        )
        .unwrap();
        let (limit, warning) = configured_limit(Some(host.path()));
        assert_eq!(limit, None);
        assert!(warning.unwrap().contains("max_concurrent"));
    }

    #[test]
    fn the_active_pool_limit_always_wins_and_a_default_is_persisted_and_reused() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let full = HostFacts {
            cpus: 12,
            memory_bytes: Some(64 * GIB),
        };
        let sandboxed = HostFacts {
            cpus: 2,
            memory_bytes: None,
        };
        // Active holders decide; a differing configured value waits for them.
        let choice = choose_limit(Some(2), Some(6), None, full);
        assert_eq!(
            (choice.limit, choice.source),
            (6, InstallCapSource::ActivePool)
        );
        assert!(choice.warning.unwrap().contains("applies once"));
        assert_eq!(choose_limit(None, Some(3), None, sandboxed).limit, 3);
        // No holder: configuration, then the default, which is persisted.
        assert_eq!(choose_limit(Some(2), None, None, full).limit, 2);
        let first = choose_limit(None, None, None, full);
        assert_eq!(first.limit, 6);
        let persisted = first.persist.unwrap();
        // A sandboxed caller with partial facts reuses it instead of 1.
        let reused = choose_limit(None, None, Some(persisted), sandboxed);
        assert_eq!((reused.limit, reused.persist), (6, None));
        // Same facts: reused. Changed, complete facts with no holder: recomputed.
        assert_eq!(
            choose_limit(None, None, Some(persisted), full).persist,
            None
        );
        let smaller = HostFacts {
            cpus: 4,
            memory_bytes: Some(64 * GIB),
        };
        let recomputed = choose_limit(None, None, Some(persisted), smaller);
        assert_eq!(recomputed.limit, 2);
        assert!(recomputed.persist.is_some());
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
        let acquire = |repository: &str, session: i64, parent: Option<&str>| {
            acquire_install_slot(
                &db,
                Some(host.path()),
                repository,
                session,
                &worktree,
                Duration::ZERO,
                parent,
            )
        };
        let (mut slot, report) = acquire("repository-a", 1, None).unwrap();
        assert_eq!(report.limit, 1);
        assert_eq!(report.limit_source, InstallCapSource::Configured);
        assert_eq!(report.other_holders, 0);
        let refusal = match acquire("repository-b", 2, None) {
            Ok(_) => panic!("a second install must wait for the only slot"),
            Err(refusal) => refusal,
        };
        assert_eq!((refusal.holders, refusal.limit), (1, 1));
        assert!(refusal.timed_out, "{refusal:?}");
        let next = refusal.next_action(2);
        assert!(
            next.contains("--session 2") && next.contains("max_concurrent"),
            "{next}"
        );

        // A nested preparation reuses its parent's slot at limit 1.
        let parent = slot.lease_id().unwrap().to_owned();
        let (nested, nested_report) = acquire("repository-a", 3, Some(&parent)).unwrap();
        assert_eq!(nested_report.limit_source, InstallCapSource::ParentSlot);
        assert_eq!(nested.lease_id(), None);
        drop(nested);
        assert!(slot.renew_best_effort(), "the parent's slot is still held");

        // A configured limit changed while a holder is active is adopted
        // from the pool for this run, with a warning, not refused.
        std::fs::write(
            host.path().join(INSTALL_CAP_FILE),
            "[installs]\nmax_concurrent = 2\n",
        )
        .unwrap();
        let refusal = match acquire("repository-b", 4, None) {
            Ok(_) => panic!("the only slot of the active pool is still held"),
            Err(refusal) => refusal,
        };
        assert_eq!(refusal.limit, 1, "{refusal:?}");
        assert!(
            refusal.timed_out,
            "an adopted limit waits, never mismatches: {refusal:?}"
        );
        slot.release();
    }

    #[test]
    fn a_failed_renewal_does_not_stop_the_install() {
        let host = tempfile::tempdir().unwrap();
        let db = host.path().join("host-resources.sqlite3");
        let (mut slot, _) = acquire_install_slot(
            &db,
            Some(host.path()),
            "repository-a",
            1,
            &host.path().join("checkout"),
            Duration::ZERO,
            None,
        )
        .unwrap();
        let grant = slot.grant.clone().unwrap();
        HostResourceCoordinator::open(&db)
            .unwrap()
            .release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
            .unwrap();
        assert!(!slot.renew_best_effort());
        assert!(!slot.renew_best_effort());
    }
}
