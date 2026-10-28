//! `gc` over this repository's managed gate cache (#295).
//!
//! Gates keep their cargo target under the per-user cache directory, at
//! `<host cache>/gates/<repository key>/<managed_cache.key>`, not beside any
//! worktree. Every byte total `gc plan` produced was rooted somewhere else, so
//! on the host behind #295 it reported `0 build caches, 209.3 KiB reclaimable`
//! while 7.7 GiB sat here -- on a machine whose gates were refusing to start
//! for want of 8 GiB, and whose refusal message pointed at `gc plan`.
//!
//! Three rules decide what may be proposed, and apply re-proves the first two
//! at the moment of removal:
//!
//! 1. **Only this repository.** Another repository's subdirectory belongs to
//!    that repository's broker, which alone can see its gates.
//! 2. **Never while a gate may be using it.** A running gate holds the host
//!    resource lease named by [`crate::gates::managed_cache_lease_name`];
//!    every run also holds owner locks and, for sessions, a pidfile. Any of
//!    those -- or an unreadable registry -- holds the entry. On 2026-09-26 a
//!    blanket delete that skipped this broke a live gate in another repository.
//! 3. **Least recently used first, down to a budget.** A warm cache is what
//!    makes gates fast; the plan keeps the most recently used entries that fit
//!    `gate_cache_bytes_budget` and proposes the rest, rather than everything.

use std::path::{Component, Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use crate::gc::{TreeRemoval, remove_condemned_tree};
use crate::{
    GcGateCacheCandidate, GcGateCacheDisposition, GcGateCacheEntry, GcGateCacheInventory, GitRepo,
};

const HEALTH_GATE_CACHE_ENTRY_BUDGET: usize = 16_384;
const MILLIS_PER_DAY: i64 = 86_400_000;

/// Where this repository's gate cache lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GateCacheLocation {
    /// `<host cache>/gates/<repository key>`.
    pub(crate) root: PathBuf,
    pub(crate) repository_key: String,
    /// The host resource registry a running gate records its cache lease in.
    /// `None` when it cannot be located, which holds every entry.
    pub(crate) registry: Option<PathBuf>,
}

/// Resolve the location using the paths captured by an owning Broker handle.
pub(crate) fn location_in(
    main_root: &Path,
    storage: &crate::gates::GateStorageLocations,
) -> Option<GateCacheLocation> {
    let repo = GitRepo::discover(main_root).ok()?;
    let repository_key = crate::gates::repository_key(&repo).0;
    let (cache, registry) = match storage {
        crate::gates::GateStorageLocations::Defaults => (
            crate::host_state::default_host_cache_dir()?,
            crate::default_host_resource_db_path().ok(),
        ),
        crate::gates::GateStorageLocations::Captured {
            host_cache_dir: Some(cache),
            host_resource_db_path,
        } => (cache.clone(), host_resource_db_path.clone()),
        crate::gates::GateStorageLocations::Captured {
            host_cache_dir: None,
            ..
        } => return None,
    };
    Some(GateCacheLocation {
        root: cache.join("gates").join(&repository_key),
        repository_key,
        registry,
    })
}

fn is_real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

fn confirmed_absent_until(path: &Path, expired: &mut impl FnMut() -> bool) -> Option<bool> {
    if expired() {
        return None;
    }
    let absent = matches!(
        std::fs::symlink_metadata(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    );
    (!expired()).then_some(absent)
}

fn cache_directory_status(
    metadata: std::io::Result<std::fs::Metadata>,
    fail_closed: bool,
) -> Result<bool, ()> {
    match metadata {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) if fail_closed => Err(()),
        Err(_) => Ok(false),
    }
}

/// `(cache key, pid)` for `.<key>.retired-<ms>-<pid>`, the name a gate gives
/// an oversized entry between renaming it aside and removing it. The
/// directory survives only when that process died in between -- or, for a
/// `gc apply` in progress, while it is still deleting.
pub(crate) fn retired_rotation(name: &str) -> Option<(&str, i64)> {
    let (key, tail) = name.strip_prefix('.')?.rsplit_once(".retired-")?;
    let (millis, pid) = tail.split_once('-')?;
    if key.is_empty() || millis.parse::<u64>().is_err() {
        return None;
    }
    Some((key, pid.parse().ok()?))
}

/// The most recent modification time among an entry, its children and its
/// grandchildren. A cargo target's own mtime moves only when a top-level
/// directory appears; `debug/`, `debug/.fingerprint` and `debug/deps` move on
/// every build. Deeper than that is a full walk, which sizing already pays for
/// but routine ranking must not.
fn last_used_at_ms(path: &Path) -> Option<i64> {
    last_used_at_ms_until(path, &mut || false).ok().flatten()
}

fn last_used_at_ms_until(
    path: &Path,
    expired: &mut impl FnMut() -> bool,
) -> Result<Option<i64>, ()> {
    fn modified_ms_until(
        path: &Path,
        expired: &mut impl FnMut() -> bool,
    ) -> Result<Option<(i64, std::fs::Metadata)>, ()> {
        if expired() {
            return Err(());
        }
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return if expired() { Err(()) } else { Ok(None) };
            }
            Err(_) => return Err(()),
        };
        if expired() {
            return Err(());
        }
        let modified = metadata.modified().map_err(|_| ())?;
        let millis = i64::try_from(
            modified
                .duration_since(UNIX_EPOCH)
                .map_err(|_| ())?
                .as_millis(),
        )
        .map_err(|_| ())?;
        if expired() {
            return Err(());
        }
        Ok(Some((millis, metadata)))
    }
    let Some((mut latest, _)) = modified_ms_until(path, expired)? else {
        return Ok(None);
    };
    let mut frontier = vec![(path.to_path_buf(), 0_usize)];
    while let Some((directory, depth)) = frontier.pop() {
        if expired() {
            return Err(());
        }
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => return Err(()),
        };
        if expired() {
            return Err(());
        }
        let mut entries = entries;
        loop {
            if expired() {
                return Err(());
            }
            let Some(entry) = entries.next() else {
                break;
            };
            let entry = entry.map_err(|_| ())?;
            if expired() {
                return Err(());
            }
            let child = entry.path();
            if let Some((modified, metadata)) = modified_ms_until(&child, expired)? {
                latest = latest.max(modified);
                if depth < 1 && metadata.is_dir() {
                    frontier.push((child, depth + 1));
                }
            }
        }
    }
    Ok(Some(latest))
}

/// What a cache entry is a generation of: its name without a trailing
/// `-v<N>` (or `v<N>`) version, so `rust-workspace-v3` and `rust-workspace-v2`
/// are one kind and a bump of the key leaves the old generation behind as an
/// older entry of the same kind.
pub(crate) fn cache_kind(name: &str) -> &str {
    let trimmed = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if trimmed.len() == name.len() {
        return name;
    }
    let Some(stem) = trimmed.strip_suffix('v') else {
        return name;
    };
    let stem = stem.strip_suffix('-').unwrap_or(stem);
    if stem.is_empty() { name } else { stem }
}

/// `Err` naming the path unless the lease registry exists as a file.
fn require_registry(registry: &Path) -> Result<(), String> {
    match std::fs::metadata(registry) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(format!(
            "lease registry at {} is not a file",
            registry.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "lease registry not found at {}; a gate cache entry exists, so a gate took a \
             lease in some registry this process cannot see",
            registry.display()
        )),
        Err(error) => Err(format!(
            "lease registry at {} cannot be read: {error}",
            registry.display()
        )),
    }
}

fn days_between(now: i64, earlier: i64) -> u32 {
    u32::try_from(now.saturating_sub(earlier).max(0) / MILLIS_PER_DAY).unwrap_or(u32::MAX)
}

/// Leases in the host registry naming one of this repository's gate caches,
/// keyed by cache key. `Err` when the registry cannot be read, which holds
/// every entry: a registry nobody can read is not evidence that no gate runs.
///
/// An *absent* registry is an error too. Every gate cache entry was created
/// by a gate that first took a lease in it, so an entry without a registry
/// means this process is looking at a different registry than the gates use
/// (another `AETHYME_HOST_STATE_DIR`, say) -- not that no gate holds anything.
/// Read-only opening would otherwise answer with an empty in-memory registry.
fn cache_leases(
    location: &GateCacheLocation,
) -> Result<std::collections::BTreeMap<String, Vec<String>>, String> {
    let prefix = crate::gates::managed_cache_lease_name(&location.repository_key, "");
    let registry = location
        .registry
        .as_deref()
        .ok_or_else(|| "the host state directory cannot be resolved".to_string())?;
    require_registry(registry)?;
    let coordinator = crate::HostResourceCoordinator::open_read_only(registry)
        .map_err(|error| error.to_string())?;
    let leases = coordinator.list(false).map_err(|error| error.to_string())?;
    let mut held = std::collections::BTreeMap::<String, Vec<String>>::new();
    for lease in leases {
        for allocation in &lease.allocations {
            if allocation.kind != "exclusive_key" {
                continue;
            }
            let Some(cache_key) = allocation.value.strip_prefix(&prefix) else {
                continue;
            };
            held.entry(cache_key.to_string()).or_default().push(format!(
                "gate cache lease {} is {} (holder pid {})",
                lease.lease_id,
                lease.state.as_str(),
                lease
                    .holder_pid
                    .map_or_else(|| "unknown".to_string(), |pid| pid.to_string()),
            ));
        }
    }
    Ok(held)
}

fn cache_leases_until(
    location: &GateCacheLocation,
    mut expired: impl FnMut() -> bool,
) -> Result<Option<std::collections::BTreeMap<String, Vec<String>>>, String> {
    let prefix = crate::gates::managed_cache_lease_name(&location.repository_key, "");
    let registry = location
        .registry
        .as_deref()
        .ok_or_else(|| "the host state directory cannot be resolved".to_string())?;
    require_registry(registry)?;
    let coordinator = crate::HostResourceCoordinator::open_read_only(registry)
        .map_err(|error| error.to_string())?;
    let Some(leases) = coordinator
        .list_until(false, HEALTH_GATE_CACHE_ENTRY_BUDGET, &mut expired)
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let mut held = std::collections::BTreeMap::<String, Vec<String>>::new();
    for lease in leases {
        if expired() {
            return Ok(None);
        }
        for allocation in &lease.allocations {
            if expired() {
                return Ok(None);
            }
            if allocation.kind != "exclusive_key" {
                continue;
            }
            let Some(cache_key) = allocation.value.strip_prefix(&prefix) else {
                continue;
            };
            held.entry(cache_key.to_string()).or_default().push(format!(
                "gate cache lease {} is {} (holder pid {})",
                lease.lease_id,
                lease.state.as_str(),
                lease
                    .holder_pid
                    .map_or_else(|| "unknown".to_string(), |pid| pid.to_string()),
            ));
        }
    }
    Ok((!expired()).then_some(held))
}

/// Exact observation time and stable age basis for a gate-cache plan.
#[derive(Clone, Copy)]
pub(crate) struct AgeReference {
    /// Exact observation time used when recording current usage.
    pub(crate) evaluated_at: i64,
    /// Stable UTC-day value used for digest-included age fields.
    pub(crate) age_anchor_ms: i64,
}

/// What decides which gate cache entries a plan keeps.
pub(crate) struct KeepRule<'a> {
    /// `gate_cache_bytes_budget`: bytes of older, idle entries to keep.
    pub(crate) budget_bytes: u64,
    /// `--include-active-gate-cache`: propose the active entries too.
    pub(crate) include_active: bool,
    /// Managed cache keys the repository's gate configuration names.
    pub(crate) configured_keys: &'a std::collections::BTreeSet<String>,
}

/// Inventory this repository's gate cache and choose what to propose.
///
/// `scan` decides whether entries are walked for their size or read from the
/// size records the last full audit left; an entry with no size is reported
/// as unmeasured and never proposed, because a budget cannot be applied to a
/// size nobody knows.
pub(crate) fn inspect(
    main_root: &Path,
    location: &GateCacheLocation,
    rule: KeepRule<'_>,
    age: AgeReference,
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
) -> (GcGateCacheInventory, Vec<GcGateCacheCandidate>) {
    let (inventory, candidates, complete) = inspect_with_expiration(
        main_root,
        location,
        InspectParameters {
            rule,
            age,
            scan,
            records,
            entry_budget: None,
            deadline: None,
        },
        || false,
    );
    debug_assert!(complete);
    (
        inventory.expect("unbounded gate-cache inspection returns an inventory"),
        candidates,
    )
}

pub(crate) fn inspect_until(
    main_root: &Path,
    location: &GateCacheLocation,
    rule: KeepRule<'_>,
    age: AgeReference,
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
    deadline: Instant,
) -> (
    Option<GcGateCacheInventory>,
    Vec<GcGateCacheCandidate>,
    bool,
) {
    inspect_with_expiration(
        main_root,
        location,
        InspectParameters {
            rule,
            age,
            scan,
            records,
            entry_budget: Some(HEALTH_GATE_CACHE_ENTRY_BUDGET),
            deadline: Some(deadline),
        },
        || Instant::now() >= deadline,
    )
}

struct InspectParameters<'a> {
    rule: KeepRule<'a>,
    age: AgeReference,
    scan: crate::SizeScan,
    records: &'a mut crate::measurement::SizeRecords,
    entry_budget: Option<usize>,
    deadline: Option<Instant>,
}

fn inspect_with_expiration(
    main_root: &Path,
    location: &GateCacheLocation,
    parameters: InspectParameters<'_>,
    mut expired: impl FnMut() -> bool,
) -> (
    Option<GcGateCacheInventory>,
    Vec<GcGateCacheCandidate>,
    bool,
) {
    let InspectParameters {
        rule,
        age,
        scan,
        records,
        entry_budget,
        deadline,
    } = parameters;
    let KeepRule {
        budget_bytes,
        include_active,
        configured_keys,
    } = rule;
    if expired() {
        return (None, Vec::new(), false);
    }
    let Some(mut holders) =
        crate::gates::running_gate_evidence_until(main_root, &mut expired, entry_budget)
    else {
        return (None, Vec::new(), false);
    };
    let leases = if entry_budget.is_some() {
        match cache_leases_until(location, &mut expired) {
            Ok(Some(leases)) => leases,
            Ok(None) => return (None, Vec::new(), false),
            Err(error) => {
                holders.push(format!(
                    "the host resource registry cannot be read, so no lease can be ruled out: {error}"
                ));
                Default::default()
            }
        }
    } else {
        match cache_leases(location) {
            Ok(leases) => leases,
            Err(error) => {
                holders.push(format!(
                    "the host resource registry cannot be read, so no lease can be ruled out: {error}"
                ));
                Default::default()
            }
        }
    };
    if expired() {
        return (None, Vec::new(), false);
    }

    let mut entries = Vec::new();
    let mut entries_examined = 0_usize;
    match std::fs::read_dir(&location.root) {
        Ok(children) => {
            for child in children {
                if expired() || entry_budget.is_some_and(|budget| entries_examined >= budget) {
                    return (None, Vec::new(), false);
                }
                let Ok(child) = child else {
                    if entry_budget.is_some() {
                        return (None, Vec::new(), false);
                    }
                    continue;
                };
                entries_examined = entries_examined.saturating_add(1);
                let path = child.path();
                // Never follow a link out of the cache root, and ignore files.
                if expired() {
                    return (None, Vec::new(), false);
                }
                let is_directory = cache_directory_status(
                    std::fs::symlink_metadata(&path),
                    entry_budget.is_some(),
                );
                if expired() {
                    return (None, Vec::new(), false);
                }
                match is_directory {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(()) => return (None, Vec::new(), false),
                }
                let entry = child.file_name().to_string_lossy().into_owned();
                let retired = retired_rotation(&entry).map(|(key, pid)| (key.to_string(), pid));
                let cache_key = retired
                    .as_ref()
                    .map_or_else(|| entry.clone(), |(key, _)| key.clone());
                let key = path.to_string_lossy().into_owned();
                let (estimated_bytes, estimated_inodes) = if scan.measures() {
                    let measured = match deadline {
                        Some(deadline) => {
                            crate::disk_headroom::directory_usage_bounded(&path, deadline)
                        }
                        None => {
                            crate::disk_headroom::directory_usage_without_following_links(&path)
                                .ok()
                        }
                    };
                    if let Some(usage) = measured {
                        records.record_usage(
                            &key,
                            usage.bytes,
                            Some(usage.inodes),
                            age.evaluated_at,
                        );
                    }
                    measured
                        .map(|usage| (Some(usage.bytes), Some(usage.inodes)))
                        .unwrap_or((None, None))
                } else {
                    records
                        .get(&key)
                        .map(|record| (Some(record.bytes), record.inodes))
                        .unwrap_or((None, None))
                };
                let last_used_at_ms = match last_used_at_ms_until(&path, &mut expired) {
                    Ok(value) => value,
                    Err(()) if entry_budget.is_some() => return (None, Vec::new(), false),
                    Err(()) => None,
                };
                let mut hold = Vec::new();
                if let Some((_, pid)) = &retired
                    && crate::broker::pid_alive(*pid)
                {
                    hold.push(format!(
                        "process {pid} is still rotating or removing this entry"
                    ));
                }
                if let Some(lease) = leases.get(&cache_key) {
                    hold.extend(lease.iter().cloned());
                }
                hold.extend(holders.iter().cloned());
                let (disposition, reason) = if !hold.is_empty() {
                    (GcGateCacheDisposition::Held, hold.join("; "))
                } else if estimated_bytes.is_none() || last_used_at_ms.is_none() {
                    (
                        GcGateCacheDisposition::Unmeasured,
                        "not sized on this pass; run `aethyme broker gc plan` to measure it".into(),
                    )
                } else if let Some((_, pid)) = &retired {
                    (
                        GcGateCacheDisposition::Reclaimable,
                        format!(
                            "a cache rotation by process {pid} was interrupted before it removed \
                         this directory; the process is gone and the rotation had already \
                         discarded it"
                        ),
                    )
                } else {
                    // Ranked against the budget below.
                    (GcGateCacheDisposition::WithinBudget, String::new())
                };
                entries.push(GcGateCacheEntry {
                    entry,
                    cache_key,
                    path: key,
                    estimated_bytes,
                    estimated_inodes,
                    last_used_at_ms,
                    // This age and its reason enter the reviewed GC digest;
                    // exact evaluation time stays on size records only.
                    age_days: last_used_at_ms.map(|at| days_between(age.age_anchor_ms, at)),
                    disposition,
                    reason,
                });
            }
        }
        Err(error) if entry_budget.is_some() && error.kind() == std::io::ErrorKind::NotFound => {
            match confirmed_absent_until(&location.root, &mut expired) {
                Some(true) => {}
                _ => return (None, Vec::new(), false),
            }
        }
        Err(_) if entry_budget.is_some() => return (None, Vec::new(), false),
        Err(_) => {}
    }
    if expired() {
        return (None, Vec::new(), false);
    }

    // The most recently used entry of each kind is the one the next gate of
    // that kind will open. Removing it frees nothing lasting -- the next gate
    // rebuilds it at the same size -- and turns that gate into a cold build.
    // So it is kept whatever the budget, unless the operator opts in.
    //
    // Which entry that is comes from the gate configuration first: a kind the
    // configuration names is active under the configured key, however its
    // modification times read -- a link created inside an old generation must
    // not make it look newer than the one gates actually open. Only a kind
    // the configuration does not name falls back to the most recent use.
    let mut newest = std::collections::BTreeMap::<String, (i64, String)>::new();
    let configured_kinds = configured_keys
        .iter()
        .map(|key| cache_kind(key).to_string())
        .collect::<std::collections::BTreeSet<_>>();
    for key in configured_keys {
        if expired() {
            return (None, Vec::new(), false);
        }
        if entries
            .iter()
            .any(|entry| entry.entry == *key && entry.last_used_at_ms.is_some())
        {
            newest.insert(cache_kind(key).to_string(), (i64::MAX, key.clone()));
        }
    }
    for entry in &entries {
        if expired() {
            return (None, Vec::new(), false);
        }
        if configured_kinds.contains(cache_kind(&entry.entry)) {
            continue;
        }
        let (Some(used), None) = (entry.last_used_at_ms, retired_rotation(&entry.entry)) else {
            continue;
        };
        let kind = cache_kind(&entry.entry).to_string();
        let candidate = (used, entry.entry.clone());
        newest
            .entry(kind)
            .and_modify(|current| {
                // Latest use wins; the higher name breaks a tie, so `-v3`
                // beats a `-v2` touched in the same instant.
                if (candidate.0, &candidate.1) > (current.0, &current.1) {
                    *current = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    for entry in &mut entries {
        if expired() {
            return (None, Vec::new(), false);
        }
        let is_newest = newest
            .get(cache_kind(&entry.entry))
            .is_some_and(|(_, name)| *name == entry.entry);
        if !is_newest || entry.disposition != GcGateCacheDisposition::WithinBudget {
            continue;
        }
        if include_active {
            entry.disposition = GcGateCacheDisposition::Reclaimable;
            entry.reason = format!(
                "the most recently used `{}` cache (used {} day(s) ago), proposed because --include-active-gate-cache was given; the next gate will rebuild it from scratch, at about the same size",
                cache_kind(&entry.entry),
                entry.age_days.unwrap_or(0)
            );
        } else {
            entry.disposition = GcGateCacheDisposition::Active;
            entry.reason = format!(
                "kept (active): the most recently used `{}` cache, which the next gate reuses; removing it frees nothing lasting and makes that gate rebuild from scratch. Pass --include-active-gate-cache to propose it anyway",
                cache_kind(&entry.entry)
            );
        }
    }

    // The budget covers only older, idle entries. Held and active entries are
    // outside it, so a gate running right now cannot push an idle entry out
    // of the budget by occupying it: what is kept for later is decided among
    // the entries nothing is using, by recency alone.
    let mut kept_bytes = 0_u64;
    if expired() {
        return (None, Vec::new(), false);
    }
    let mut ranked = entries
        .iter_mut()
        .filter(|entry| {
            entry.disposition == GcGateCacheDisposition::WithinBudget
                && retired_rotation(&entry.entry).is_none()
        })
        .collect::<Vec<_>>();
    if expired() {
        return (None, Vec::new(), false);
    }
    ranked.sort_by(|left, right| {
        right
            .last_used_at_ms
            .cmp(&left.last_used_at_ms)
            .then_with(|| left.entry.cmp(&right.entry))
    });
    // Strict LRU: once one entry does not fit, it and everything used less
    // recently are proposed, even an older entry small enough to squeeze in.
    let mut overflowed = false;
    for entry in ranked {
        if expired() {
            return (None, Vec::new(), false);
        }
        let bytes = entry.estimated_bytes.unwrap_or(0);
        if !overflowed && kept_bytes.saturating_add(bytes) <= budget_bytes {
            kept_bytes = kept_bytes.saturating_add(bytes);
            entry.reason = format!(
                "an older entry used {} day(s) ago that fits the {} byte budget for older \
                 gate caches; kept warm",
                entry.age_days.unwrap_or(0),
                budget_bytes
            );
        } else {
            overflowed = true;
            entry.disposition = GcGateCacheDisposition::Reclaimable;
            entry.reason = format!(
                "an older entry, least recently used ({} day(s) idle), beyond the {} byte \
                 budget for older gate caches, and no running gate holds it",
                entry.age_days.unwrap_or(0),
                budget_bytes
            );
        }
    }

    if expired() {
        return (None, Vec::new(), false);
    }
    entries.sort_by(|left, right| left.entry.cmp(&right.entry));
    let mut candidates = entries
        .iter()
        .filter(|entry| entry.disposition == GcGateCacheDisposition::Reclaimable)
        .map(|entry| GcGateCacheCandidate {
            entry: entry.entry.clone(),
            cache_key: entry.cache_key.clone(),
            path: entry.path.clone(),
            estimated_bytes: entry.estimated_bytes.unwrap_or(0),
            estimated_inodes: entry.estimated_inodes,
            last_used_at_ms: entry.last_used_at_ms.unwrap_or(0),
            age_days: entry.age_days.unwrap_or(0),
            reason: entry.reason.clone(),
        })
        .collect::<Vec<_>>();
    if expired() {
        return (None, Vec::new(), false);
    }
    // Interrupted rotations first -- they are already discarded, not a cache
    // anyone could reuse -- then least recently used, so a bounded apply
    // reaches the coldest bytes before the warmer ones.
    candidates.sort_by(|left, right| {
        let live = |candidate: &GcGateCacheCandidate| retired_rotation(&candidate.entry).is_none();
        live(left)
            .cmp(&live(right))
            .then_with(|| left.last_used_at_ms.cmp(&right.last_used_at_ms))
            .then_with(|| left.entry.cmp(&right.entry))
    });
    let sum = |disposition: GcGateCacheDisposition| {
        entries
            .iter()
            .filter(|entry| entry.disposition == disposition)
            .filter_map(|entry| entry.estimated_bytes)
            .fold(0_u64, u64::saturating_add)
    };
    let sum_known_inodes = |values: Vec<Option<u64>>| {
        values.into_iter().try_fold(0_u64, |total, count| {
            count.map(|count| total.saturating_add(count))
        })
    };
    let sum_inodes = |disposition: GcGateCacheDisposition| {
        sum_known_inodes(
            entries
                .iter()
                .filter(|entry| entry.disposition == disposition)
                .map(|entry| entry.estimated_inodes)
                .collect(),
        )
    };
    let inventory = GcGateCacheInventory {
        root: location.root.to_string_lossy().into_owned(),
        repository_key: location.repository_key.clone(),
        budget_bytes,
        total_bytes: entries
            .iter()
            .filter_map(|entry| entry.estimated_bytes)
            .fold(0_u64, u64::saturating_add),
        total_inodes: sum_known_inodes(
            entries.iter().map(|entry| entry.estimated_inodes).collect(),
        ),
        reclaimable_bytes: sum(GcGateCacheDisposition::Reclaimable),
        reclaimable_inodes: sum_inodes(GcGateCacheDisposition::Reclaimable),
        held_bytes: sum(GcGateCacheDisposition::Held),
        held_inodes: sum_inodes(GcGateCacheDisposition::Held),
        active_bytes: sum(GcGateCacheDisposition::Active),
        active_inodes: sum_inodes(GcGateCacheDisposition::Active),
        include_active,
        holders,
        entries,
    };
    (Some(inventory), candidates, true)
}

/// What removing one candidate achieved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateCacheReclaim {
    /// Removed; the bytes were measured immediately before removal.
    Removed(u64),
    /// Already gone. Nothing was freed by this run.
    Gone,
    /// The deadline passed mid-removal. The entry was already renamed aside
    /// as an interrupted rotation under this process's pid, so no gate will
    /// use it again and the next plan proposes the remainder.
    Interrupted,
}

/// Remove one reviewed candidate using an explicit location and registry,
/// re-proving at the moment of removal that no gate is using it.
///
/// The plan may be minutes old. So the entry's own lease is *taken* -- a gate
/// that starts now waits for it rather than racing the removal -- and the
/// pidfiles and owner locks are read again under it. A live entry is then
/// renamed aside, exactly as a gate rotates an oversized cache, and the lease
/// is released before the slow part: the next gate gets a fresh directory at
/// once instead of waiting minutes for an unlink.
pub(crate) fn reclaim_in(
    main_root: &Path,
    location: &GateCacheLocation,
    candidate: &GcGateCacheCandidate,
    deadline: Option<Instant>,
) -> Result<GateCacheReclaim, String> {
    let registry = location
        .registry
        .as_deref()
        .ok_or_else(|| "the host resource registry cannot be located".to_string())?;
    let mut components = Path::new(&candidate.entry).components();
    let single =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    let path = location.root.join(&candidate.entry);
    if !single || Path::new(&candidate.path) != path {
        return Err(format!(
            "{} is not an entry of this repository's gate cache at {}; review a new GC plan",
            candidate.path,
            location.root.display()
        ));
    }
    if !is_real_directory(&path) {
        return Ok(GateCacheReclaim::Gone);
    }
    let retired = retired_rotation(&candidate.entry);
    let expected_key = retired.map_or(candidate.entry.as_str(), |(key, _)| key);
    if expected_key != candidate.cache_key {
        return Err(format!(
            "{}: its cache key changed; review a new GC plan",
            candidate.path
        ));
    }
    if let Some((_, pid)) = retired
        && crate::broker::pid_alive(pid)
    {
        return Err(format!(
            "{}: process {pid} is rotating or removing it",
            candidate.path
        ));
    }

    // `open` would create a missing registry, handing gc a lease in a
    // registry no gate reads and proving nothing.
    require_registry(registry).map_err(|reason| format!("{}: {reason}", candidate.path))?;
    let mut coordinator =
        crate::HostResourceCoordinator::open(registry).map_err(|error| error.to_string())?;
    let request = crate::HostResourceRequest {
        schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
        request_id: format!(
            "gc-gate-cache-{}-{}-{}",
            std::process::id(),
            crate::clock::epoch_ms(),
            candidate.entry
        ),
        repository: location.repository_key.clone(),
        worktree_fingerprint: "gc".into(),
        run_id: format!("gc-{}", std::process::id()),
        ttl_seconds: 300,
        holder_pid: Some(std::process::id()),
        resources: vec![crate::HostResourceRequirement {
            key: "managed_cache".into(),
            resource: crate::HostResourceKind::ExclusiveKey {
                name: crate::gates::managed_cache_lease_name(
                    &location.repository_key,
                    &candidate.cache_key,
                ),
            },
        }],
    };
    let grant = coordinator.acquire(&request).map_err(|error| {
        format!(
            "{}: a gate holds this cache ({error}); it was left in place",
            candidate.path
        )
    })?;
    let release = |coordinator: &mut crate::HostResourceCoordinator| {
        crate::warn_unrecorded(
            "release the gate cache lease gc took",
            coordinator.release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            ),
        );
    };
    let evidence = crate::gates::running_gate_evidence(main_root);
    if !evidence.is_empty() {
        release(&mut coordinator);
        return Err(format!(
            "{}: a gate of this repository is running ({}); it was left in place",
            candidate.path,
            evidence.join("; ")
        ));
    }
    // The entry must still be the one the operator reviewed. A gate that ran
    // since the plan -- or since a journal recorded it -- changed its last use
    // or its size, and removing it now would discard a cache that became warm
    // again. Checked under the lease, so no gate can change it after this.
    let current_used = last_used_at_ms(&path);
    let current_usage = crate::disk_headroom::directory_usage_without_following_links(&path).ok();
    let current_bytes = current_usage.map(|usage| usage.bytes);
    let current_inodes = current_usage.map(|usage| usage.inodes);
    if current_used != Some(candidate.last_used_at_ms)
        || current_bytes != Some(candidate.estimated_bytes)
        // A plan written before inode counts existed carries none; it is
        // still checked on bytes and last use.
        || candidate
            .estimated_inodes
            .is_some_and(|planned| current_inodes != Some(planned))
    {
        release(&mut coordinator);
        return Err(format!(
            "{}: changed since the plan (last used {:?} -> {:?}, {} -> {:?} bytes); it was \
             left in place, review a new GC plan",
            candidate.path,
            candidate.last_used_at_ms,
            current_used,
            candidate.estimated_bytes,
            current_bytes
        ));
    }
    let condemned = if retired.is_some() {
        path
    } else {
        let aside = location.root.join(format!(
            ".{}.retired-{}-{}",
            candidate.cache_key,
            crate::clock::epoch_ms(),
            std::process::id()
        ));
        if let Err(error) = std::fs::rename(&path, &aside) {
            release(&mut coordinator);
            return Err(format!("{}: cannot rename aside: {error}", candidate.path));
        }
        aside
    };
    release(&mut coordinator);
    let bytes = candidate.estimated_bytes;
    match remove_condemned_tree(&condemned, None, deadline) {
        Ok(TreeRemoval::Complete) => Ok(GateCacheReclaim::Removed(bytes)),
        Ok(TreeRemoval::Interrupted) => Ok(GateCacheReclaim::Interrupted),
        Err(error) => Err(format!("{}: {error}", condemned.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository runtime directory, a gate cache root holding one entry of
    /// `bytes`, and a private host resource registry -- no environment.
    struct Fixture {
        _tmp: tempfile::TempDir,
        main_root: PathBuf,
        location: GateCacheLocation,
    }

    fn fixture(entry: &str, bytes: usize) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let main_root = tmp.path().join("repo");
        std::fs::create_dir_all(main_root.join(".aethyme/run/gates/owners")).unwrap();
        let root = tmp.path().join("cache/gates/rk");
        std::fs::create_dir_all(root.join(entry).join("debug")).unwrap();
        std::fs::write(root.join(entry).join("debug/blob"), vec![b'x'; bytes]).unwrap();
        let registry = tmp.path().join("state/host-resources.db");
        // Gates create the registry before any cache entry exists.
        crate::HostResourceCoordinator::open(&registry).unwrap();
        let location = GateCacheLocation {
            root,
            repository_key: "rk".into(),
            registry: Some(registry),
        };
        Fixture {
            _tmp: tmp,
            main_root,
            location,
        }
    }

    fn candidate(fixture: &Fixture, entry: &str) -> GcGateCacheCandidate {
        let usage = crate::disk_headroom::directory_usage_without_following_links(
            &fixture.location.root.join(entry),
        )
        .expect("fixture cache can be measured");
        GcGateCacheCandidate {
            entry: entry.into(),
            cache_key: retired_rotation(entry).map_or(entry, |(key, _)| key).into(),
            path: fixture
                .location
                .root
                .join(entry)
                .to_string_lossy()
                .into_owned(),
            // What a plan measured a moment ago.
            estimated_bytes: usage.bytes,
            estimated_inodes: Some(usage.inodes),
            last_used_at_ms: last_used_at_ms(&fixture.location.root.join(entry)).unwrap_or(0),
            age_days: 0,
            reason: String::new(),
        }
    }

    fn inspect_fixture(
        fixture: &Fixture,
        configured: &[&str],
    ) -> (GcGateCacheInventory, Vec<GcGateCacheCandidate>) {
        let mut records = crate::measurement::SizeRecords::default();
        let evaluated_at = crate::clock::epoch_ms();
        inspect(
            &fixture.main_root,
            &fixture.location,
            KeepRule {
                budget_bytes: 0,
                include_active: false,
                configured_keys: &configured.iter().map(|key| key.to_string()).collect(),
            },
            AgeReference {
                evaluated_at,
                age_anchor_ms: evaluated_at.div_euclid(MILLIS_PER_DAY) * MILLIS_PER_DAY,
            },
            crate::SizeScan::Measure,
            &mut records,
        )
    }

    fn set_tree_modified_at(path: &Path, modified: std::time::SystemTime) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let child = entry.path();
            if entry.file_type().unwrap().is_dir() {
                set_tree_modified_at(&child, modified);
            }
            std::fs::File::open(child)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }
        std::fs::File::open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    #[test]
    fn gate_cache_candidate_age_uses_the_gc_day_anchor() {
        let fixture = fixture("rust-workspace-v3", 10);
        let day_start = 100 * MILLIS_PER_DAY;
        let last_used_at = day_start - 60 * 60 * 1000;
        set_tree_modified_at(
            &fixture.location.root.join("rust-workspace-v3"),
            UNIX_EPOCH + std::time::Duration::from_millis(last_used_at as u64),
        );

        let evaluate = |evaluated_at| {
            let mut records = crate::measurement::SizeRecords::default();
            inspect(
                &fixture.main_root,
                &fixture.location,
                KeepRule {
                    budget_bytes: 0,
                    include_active: true,
                    configured_keys: &std::collections::BTreeSet::new(),
                },
                AgeReference {
                    evaluated_at,
                    age_anchor_ms: day_start,
                },
                crate::SizeScan::Measure,
                &mut records,
            )
            .1
        };

        let early_evaluation = day_start + 60 * 60 * 1000;
        let late_evaluation = day_start + 23 * 60 * 60 * 1000;
        assert_ne!(
            days_between(early_evaluation, last_used_at),
            days_between(late_evaluation, last_used_at),
            "exact evaluation time crosses a displayed age-day boundary"
        );

        let early = evaluate(early_evaluation);
        let late = evaluate(late_evaluation);
        assert_eq!(early, late);
        assert_eq!(early[0].age_days, 0);
        assert!(early[0].reason.contains("used 0 day(s) ago"));
    }

    #[test]
    fn gate_cache_health_inventory_discards_results_after_deadline() {
        let fixture = fixture("rust-workspace-v3", 10);
        let mut records = crate::measurement::SizeRecords::default();
        let evaluated_at = crate::clock::epoch_ms();
        let (inventory, candidates, complete) = inspect_until(
            &fixture.main_root,
            &fixture.location,
            KeepRule {
                budget_bytes: 0,
                include_active: false,
                configured_keys: &std::collections::BTreeSet::new(),
            },
            AgeReference {
                evaluated_at,
                age_anchor_ms: evaluated_at.div_euclid(MILLIS_PER_DAY) * MILLIS_PER_DAY,
            },
            crate::SizeScan::Recorded,
            &mut records,
            Instant::now() - std::time::Duration::from_millis(1),
        );

        assert!(!complete);
        assert!(inventory.is_none());
        assert!(candidates.is_empty());
    }

    #[test]
    fn bounded_gate_cache_inventory_accepts_a_confirmed_absent_root() {
        let fixture = fixture("rust-workspace-v3", 10);
        std::fs::remove_dir_all(&fixture.location.root).unwrap();
        let mut records = crate::measurement::SizeRecords::default();
        let evaluated_at = crate::clock::epoch_ms();
        let (inventory, candidates, complete) = inspect_until(
            &fixture.main_root,
            &fixture.location,
            KeepRule {
                budget_bytes: 0,
                include_active: false,
                configured_keys: &std::collections::BTreeSet::new(),
            },
            AgeReference {
                evaluated_at,
                age_anchor_ms: evaluated_at.div_euclid(MILLIS_PER_DAY) * MILLIS_PER_DAY,
            },
            crate::SizeScan::Recorded,
            &mut records,
            Instant::now() + std::time::Duration::from_secs(1),
        );

        assert!(complete);
        assert!(candidates.is_empty());
        let inventory = inventory.expect("confirmed absence is a complete empty inventory");
        assert!(inventory.entries.is_empty());
        assert_eq!(inventory.total_bytes, 0);
    }

    #[test]
    fn last_used_scan_stops_when_deadline_expires_mid_directory() {
        let fixture = fixture("rust-workspace-v3", 10);
        let path = fixture.location.root.join("rust-workspace-v3");
        let mut checks = 0;
        let result = last_used_at_ms_until(&path, &mut || {
            checks += 1;
            checks >= 4
        });

        assert_eq!(result, Err(()));
        assert_eq!(checks, 4, "the scan checks its bound between entries");
    }

    #[test]
    fn last_used_scan_fails_closed_on_enumeration_errors_but_accepts_absence() {
        let tmp = tempfile::tempdir().unwrap();
        let not_a_directory = tmp.path().join("cache-entry");
        std::fs::write(&not_a_directory, b"not a directory").unwrap();
        assert_eq!(
            last_used_at_ms_until(&not_a_directory, &mut || false),
            Err(()),
            "a partial mtime inventory must not rank a cache as stale"
        );

        assert_eq!(
            last_used_at_ms_until(&tmp.path().join("absent"), &mut || false),
            Ok(None),
            "a positively absent cache path has no inventory"
        );
    }

    #[test]
    fn bounded_cache_directory_probe_defers_on_metadata_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let directory = tmp.path().join("cache");
        let file = tmp.path().join("not-a-cache");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(&file, b"file").unwrap();

        assert_eq!(
            cache_directory_status(std::fs::symlink_metadata(&directory), true),
            Ok(true)
        );
        assert_eq!(
            cache_directory_status(std::fs::symlink_metadata(&file), true),
            Ok(false)
        );
        assert_eq!(
            cache_directory_status(
                Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
                true
            ),
            Ok(false)
        );
        assert_eq!(
            cache_directory_status(
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
                true
            ),
            Err(())
        );
        assert_eq!(
            cache_directory_status(
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
                false
            ),
            Ok(false),
            "the unbounded planner keeps its prior best-effort behavior"
        );
    }

    /// Finding 1: an absent registry is a registry this process cannot see,
    /// not proof that no gate holds anything. Every entry is held with the
    /// path named, and apply neither removes the entry nor creates a
    /// registry as a side effect.
    #[test]
    fn an_absent_registry_holds_every_entry_and_apply_creates_none() {
        let mut fixture = fixture("rust-workspace-v2", 10);
        std::fs::create_dir_all(fixture.location.root.join("rust-workspace-v3")).unwrap();
        let absent = fixture.location.root.join("elsewhere/host-resources.db");
        fixture.location.registry = Some(absent.clone());
        let (inventory, candidates) = inspect_fixture(&fixture, &[]);
        assert!(candidates.is_empty(), "{candidates:?}");
        assert!(inventory.total_inodes.unwrap() > 0, "{inventory:#?}");
        assert!(inventory.held_inodes.unwrap() > 0, "{inventory:#?}");
        assert!(
            inventory
                .entries
                .iter()
                .all(|entry| entry.disposition == GcGateCacheDisposition::Held),
            "{inventory:#?}"
        );
        assert!(
            inventory.holders.iter().any(|holder| holder
                .contains(&format!("lease registry not found at {}", absent.display()))),
            "{:?}",
            inventory.holders
        );
        let error = reclaim_in(
            &fixture.main_root,
            &fixture.location,
            &candidate(&fixture, "rust-workspace-v2"),
            None,
        )
        .unwrap_err();
        assert!(error.contains("lease registry not found"), "{error}");
        assert!(!absent.exists(), "apply must not create a registry");
        assert!(
            fixture
                .location
                .root
                .join("rust-workspace-v2/debug/blob")
                .is_file()
        );
    }

    /// Finding 2: an entry that changed after the plan -- a gate ran in it,
    /// so its size or last use moved -- is not the entry that was reviewed.
    #[test]
    fn apply_leaves_an_entry_that_changed_since_the_plan() {
        let grown = fixture("rust-workspace-v2", 1000);
        let reviewed = candidate(&grown, "rust-workspace-v2");
        std::fs::write(
            grown.location.root.join("rust-workspace-v2/debug/more"),
            vec![b'y'; 10],
        )
        .unwrap();
        let error = reclaim_in(&grown.main_root, &grown.location, &reviewed, None).unwrap_err();
        assert!(error.contains("changed since the plan"), "{error}");
        assert!(
            grown
                .location
                .root
                .join("rust-workspace-v2/debug/more")
                .is_file()
        );

        // Same bytes, one more empty directory: inode drift is a changed
        // entry even though the byte estimate stayed identical.
        let inode_changed = fixture("rust-workspace-v2", 1000);
        let reviewed = candidate(&inode_changed, "rust-workspace-v2");
        let cache_entry = inode_changed.location.root.join("rust-workspace-v2");
        let debug = cache_entry.join("debug");
        let empty = debug.join("empty");
        std::fs::create_dir(&empty).unwrap();
        let unchanged_mtime =
            UNIX_EPOCH + std::time::Duration::from_millis(reviewed.last_used_at_ms as u64);
        std::fs::File::open(&empty)
            .unwrap()
            .set_modified(unchanged_mtime)
            .unwrap();
        std::fs::File::open(&debug)
            .unwrap()
            .set_modified(unchanged_mtime)
            .unwrap();
        std::fs::File::open(&cache_entry)
            .unwrap()
            .set_modified(unchanged_mtime)
            .unwrap();
        let after =
            crate::disk_headroom::directory_usage_without_following_links(&cache_entry).unwrap();
        assert_eq!(after.bytes, reviewed.estimated_bytes);
        assert_eq!(after.inodes, reviewed.estimated_inodes.unwrap() + 1);
        assert_eq!(
            last_used_at_ms(&cache_entry),
            Some(reviewed.last_used_at_ms),
            "the reviewed use time is unchanged too"
        );
        let error = reclaim_in(
            &inode_changed.main_root,
            &inode_changed.location,
            &reviewed,
            None,
        )
        .unwrap_err();
        assert!(error.contains("changed since the plan"), "{error}");
        assert!(
            inode_changed
                .location
                .root
                .join("rust-workspace-v2/debug/empty")
                .is_dir()
        );

        // Same size and inode count, later use: still not the reviewed entry.
        let used = fixture("rust-workspace-v2", 1000);
        let mut reviewed = candidate(&used, "rust-workspace-v2");
        reviewed.last_used_at_ms -= 60_000;
        let error = reclaim_in(&used.main_root, &used.location, &reviewed, None).unwrap_err();
        assert!(error.contains("changed since the plan"), "{error}");
        assert!(
            used.location
                .root
                .join("rust-workspace-v2/debug/blob")
                .is_file()
        );
    }

    /// Finding 3: the configured key is the active cache of its kind, even
    /// when an older generation carries the newer modification time.
    #[test]
    fn the_configured_key_is_active_whatever_the_mtimes_say() {
        let fixture = fixture("rust-workspace-v3", 10);
        let old = fixture.location.root.join("rust-workspace-v3");
        std::fs::File::open(old.join("debug/blob"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(86_400))
            .unwrap();
        std::fs::File::open(old.join("debug"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(86_400))
            .unwrap();
        std::fs::File::open(&old)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(86_400))
            .unwrap();
        // An older generation touched just now.
        std::fs::create_dir_all(fixture.location.root.join("rust-workspace-v2/debug")).unwrap();
        std::fs::write(
            fixture.location.root.join("rust-workspace-v2/debug/link"),
            "x",
        )
        .unwrap();

        let (inventory, _) = inspect_fixture(&fixture, &["rust-workspace-v3"]);
        let disposition = |name: &str| {
            inventory
                .entries
                .iter()
                .find(|entry| entry.entry == name)
                .unwrap()
                .disposition
        };
        assert_eq!(
            disposition("rust-workspace-v3"),
            GcGateCacheDisposition::Active
        );
        assert_eq!(
            disposition("rust-workspace-v2"),
            GcGateCacheDisposition::Reclaimable
        );

        // With no configuration the fallback is most recent use.
        let (inventory, _) = inspect_fixture(&fixture, &[]);
        assert_eq!(
            inventory
                .entries
                .iter()
                .find(|entry| entry.entry == "rust-workspace-v2")
                .unwrap()
                .disposition,
            GcGateCacheDisposition::Active
        );
    }

    fn hold_lease(fixture: &Fixture, cache_key: &str) -> crate::HostResourceCoordinator {
        let mut coordinator =
            crate::HostResourceCoordinator::open(fixture.location.registry.as_deref().unwrap())
                .unwrap();
        coordinator
            .acquire(&crate::HostResourceRequest {
                schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
                request_id: "a-running-gate".into(),
                repository: "rk".into(),
                worktree_fingerprint: "wt".into(),
                run_id: "gate".into(),
                ttl_seconds: 600,
                holder_pid: Some(std::process::id()),
                resources: vec![crate::HostResourceRequirement {
                    key: "managed_cache".into(),
                    resource: crate::HostResourceKind::ExclusiveKey {
                        name: crate::gates::managed_cache_lease_name("rk", cache_key),
                    },
                }],
            })
            .unwrap();
        coordinator
    }

    /// (a) at apply time: the plan may be minutes old, and a gate that took
    /// the entry's lease since must win. The entry is left exactly in place.
    #[test]
    fn apply_leaves_an_entry_whose_lease_a_gate_now_holds() {
        let fixture = fixture("rust-workspace-v3", 1000);
        let _gate = hold_lease(&fixture, "rust-workspace-v3");
        let error = reclaim_in(
            &fixture.main_root,
            &fixture.location,
            &candidate(&fixture, "rust-workspace-v3"),
            None,
        )
        .unwrap_err();
        assert!(error.contains("a gate holds this cache"), "{error}");
        assert!(
            fixture
                .location
                .root
                .join("rust-workspace-v3/debug/blob")
                .is_file()
        );
    }

    /// (a) at apply time, for the witnesses that are not leases: a held owner
    /// lock or a live pidfile means a gate of this repository is running.
    #[test]
    fn apply_leaves_every_entry_while_a_gate_owner_lock_or_pidfile_is_live() {
        let fixture = fixture("rust-workspace-v3", 1000);
        let run_dir = fixture.main_root.join(".aethyme/run/gates");

        let lock = std::fs::File::create(run_dir.join("owners/rust-all-0.lock")).unwrap();
        lock.lock().unwrap();
        let error = reclaim_in(
            &fixture.main_root,
            &fixture.location,
            &candidate(&fixture, "rust-workspace-v3"),
            None,
        )
        .unwrap_err();
        assert!(error.contains("owner lock"), "{error}");
        lock.unlock().unwrap();

        let pid = std::process::id();
        std::fs::write(run_dir.join("7-rust.pid"), format!("{pid} tree {pid} -")).unwrap();
        let error = reclaim_in(
            &fixture.main_root,
            &fixture.location,
            &candidate(&fixture, "rust-workspace-v3"),
            None,
        )
        .unwrap_err();
        assert!(error.contains("pidfile"), "{error}");
        assert!(
            fixture
                .location
                .root
                .join("rust-workspace-v3/debug/blob")
                .is_file()
        );

        // The lease gc took to decide was released on every refusal.
        let coordinator =
            crate::HostResourceCoordinator::open(fixture.location.registry.as_deref().unwrap())
                .unwrap();
        assert!(coordinator.list(false).unwrap().is_empty());
    }

    /// (f) at apply time: the bytes reported are the bytes that were on disk
    /// immediately before removal, nothing is left renamed aside, and the
    /// lease is released for the next gate.
    #[test]
    fn apply_removes_an_idle_entry_and_reports_its_bytes() {
        let fixture = fixture("rust-workspace-v3", 1234);
        let outcome = reclaim_in(
            &fixture.main_root,
            &fixture.location,
            &candidate(&fixture, "rust-workspace-v3"),
            None,
        )
        .unwrap();
        assert_eq!(outcome, GateCacheReclaim::Removed(1234));
        assert_eq!(
            std::fs::read_dir(&fixture.location.root).unwrap().count(),
            0,
            "neither the entry nor a renamed-aside copy may remain"
        );
        let coordinator =
            crate::HostResourceCoordinator::open(fixture.location.registry.as_deref().unwrap())
                .unwrap();
        assert!(coordinator.list(false).unwrap().is_empty());
    }

    /// (b) at apply time: a journal naming a path outside this repository's
    /// root is refused, even when the entry name matches.
    #[test]
    fn apply_refuses_a_path_outside_this_repository_cache() {
        let fixture = fixture("rust-workspace-v3", 10);
        let other = fixture
            .location
            .root
            .parent()
            .unwrap()
            .join("other-repo/rust-workspace-v3");
        std::fs::create_dir_all(&other).unwrap();
        let mut foreign = candidate(&fixture, "rust-workspace-v3");
        foreign.path = other.to_string_lossy().into_owned();
        let error = reclaim_in(&fixture.main_root, &fixture.location, &foreign, None).unwrap_err();
        assert!(
            error.contains("is not an entry of this repository"),
            "{error}"
        );
        assert!(other.is_dir());
        let mut escaping = candidate(&fixture, "../other-repo/rust-workspace-v3");
        escaping.path = other.to_string_lossy().into_owned();
        assert!(reclaim_in(&fixture.main_root, &fixture.location, &escaping, None).is_err());
        assert!(other.is_dir());
    }

    /// An unreadable registry is not evidence that no gate runs.
    #[test]
    fn an_unlocatable_registry_holds_every_entry() {
        let mut fixture = fixture("rust-workspace-v3", 10);
        fixture.location.registry = None;
        let mut records = crate::measurement::SizeRecords::default();
        let evaluated_at = crate::clock::epoch_ms();
        let (inventory, candidates) = inspect(
            &fixture.main_root,
            &fixture.location,
            KeepRule {
                budget_bytes: 0,
                include_active: false,
                configured_keys: &Default::default(),
            },
            AgeReference {
                evaluated_at,
                age_anchor_ms: evaluated_at.div_euclid(MILLIS_PER_DAY) * MILLIS_PER_DAY,
            },
            crate::SizeScan::Measure,
            &mut records,
        );
        assert!(candidates.is_empty(), "{candidates:?}");
        assert_eq!(
            inventory.entries[0].disposition,
            GcGateCacheDisposition::Held
        );
        assert!(
            reclaim_in(
                &fixture.main_root,
                &fixture.location,
                &candidate(&fixture, "rust-workspace-v3"),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn a_cache_kind_drops_only_a_trailing_version() {
        assert_eq!(cache_kind("rust-workspace-v3"), "rust-workspace");
        assert_eq!(cache_kind("rust-workspace-v12"), "rust-workspace");
        assert_eq!(cache_kind("rust-workspacev2"), "rust-workspace");
        assert_eq!(cache_kind("py-tools"), "py-tools");
        assert_eq!(cache_kind("node-18"), "node-18");
        assert_eq!(cache_kind("v3"), "v3");
    }

    #[test]
    fn a_retired_rotation_name_yields_its_key_and_pid() {
        assert_eq!(
            retired_rotation(".rust-workspace-v3.retired-1700000000000-4242"),
            Some(("rust-workspace-v3", 4242))
        );
        assert_eq!(retired_rotation("rust-workspace-v3"), None);
        assert_eq!(retired_rotation(".rust.retired-x-1"), None);
        assert_eq!(retired_rotation(".retired-1-2"), None);
        assert_eq!(retired_rotation(".k.retired-1-notapid"), None);
    }
}
