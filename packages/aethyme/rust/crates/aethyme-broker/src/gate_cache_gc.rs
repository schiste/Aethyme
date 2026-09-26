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

use crate::broker::directory_size_without_following_links;
use crate::gc::{TreeRemoval, remove_condemned_tree};
use crate::{
    GcGateCacheCandidate, GcGateCacheDisposition, GcGateCacheEntry, GcGateCacheInventory, GitRepo,
};

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

/// Resolve the location exactly as a gate does: the same repository key
/// (from the main checkout, #170) under the same per-user cache directory.
pub(crate) fn location(main_root: &Path) -> Option<GateCacheLocation> {
    let repo = GitRepo::discover(main_root).ok()?;
    let repository_key = crate::gates::repository_key(&repo).0;
    let cache = crate::host_state::default_host_cache_dir()?;
    Some(GateCacheLocation {
        root: cache.join("gates").join(&repository_key),
        repository_key,
        registry: crate::default_host_resource_db_path().ok(),
    })
}

fn is_real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
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
    fn modified_ms(path: &Path) -> Option<i64> {
        let modified = std::fs::symlink_metadata(path).ok()?.modified().ok()?;
        i64::try_from(modified.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()
    }
    let mut latest = modified_ms(path)?;
    let mut frontier = vec![(path.to_path_buf(), 0_usize)];
    while let Some((directory, depth)) = frontier.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let child = entry.path();
            if let Some(modified) = modified_ms(&child) {
                latest = latest.max(modified);
            }
            if depth < 1 && is_real_directory(&child) {
                frontier.push((child, depth + 1));
            }
        }
    }
    Some(latest)
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
    u32::try_from(now.saturating_sub(earlier).max(0) / 86_400_000).unwrap_or(u32::MAX)
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

/// Inventory this repository's gate cache and choose what to propose.
///
/// `scan` decides whether entries are walked for their size or read from the
/// size records the last full audit left; an entry with no size is reported
/// as unmeasured and never proposed, because a budget cannot be applied to a
/// size nobody knows.
/// What decides which gate cache entries a plan keeps.
pub(crate) struct KeepRule<'a> {
    /// `gate_cache_bytes_budget`: bytes of older, idle entries to keep.
    pub(crate) budget_bytes: u64,
    /// `--include-active-gate-cache`: propose the active entries too.
    pub(crate) include_active: bool,
    /// Managed cache keys the repository's gate configuration names.
    pub(crate) configured_keys: &'a std::collections::BTreeSet<String>,
}

pub(crate) fn inspect(
    main_root: &Path,
    location: &GateCacheLocation,
    rule: KeepRule<'_>,
    evaluated_at: i64,
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
) -> (GcGateCacheInventory, Vec<GcGateCacheCandidate>) {
    let KeepRule {
        budget_bytes,
        include_active,
        configured_keys,
    } = rule;
    let mut holders = crate::gates::running_gate_evidence(main_root);
    let leases = match cache_leases(location) {
        Ok(leases) => leases,
        Err(error) => {
            holders.push(format!(
                "the host resource registry cannot be read, so no lease can be ruled out: {error}"
            ));
            Default::default()
        }
    };

    let mut entries = Vec::new();
    if let Ok(children) = std::fs::read_dir(&location.root) {
        for child in children.flatten() {
            let path = child.path();
            // Never follow a link out of the cache root, and ignore files.
            if !is_real_directory(&path) {
                continue;
            }
            let entry = child.file_name().to_string_lossy().into_owned();
            let retired = retired_rotation(&entry).map(|(key, pid)| (key.to_string(), pid));
            let cache_key = retired
                .as_ref()
                .map_or_else(|| entry.clone(), |(key, _)| key.clone());
            let key = path.to_string_lossy().into_owned();
            let estimated_bytes = if scan.measures() {
                let measured = directory_size_without_following_links(&path).ok();
                if let Some(bytes) = measured {
                    records.record(&key, bytes, evaluated_at);
                }
                measured
            } else {
                records.get(&key).map(|record| record.bytes)
            };
            let last_used_at_ms = last_used_at_ms(&path);
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
                last_used_at_ms,
                age_days: last_used_at_ms.map(|at| days_between(evaluated_at, at)),
                disposition,
                reason,
            });
        }
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
        if entries
            .iter()
            .any(|entry| entry.entry == *key && entry.last_used_at_ms.is_some())
        {
            newest.insert(cache_kind(key).to_string(), (i64::MAX, key.clone()));
        }
    }
    for entry in &entries {
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
    let mut ranked = entries
        .iter_mut()
        .filter(|entry| {
            entry.disposition == GcGateCacheDisposition::WithinBudget
                && retired_rotation(&entry.entry).is_none()
        })
        .collect::<Vec<_>>();
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

    entries.sort_by(|left, right| left.entry.cmp(&right.entry));
    let mut candidates = entries
        .iter()
        .filter(|entry| entry.disposition == GcGateCacheDisposition::Reclaimable)
        .map(|entry| GcGateCacheCandidate {
            entry: entry.entry.clone(),
            cache_key: entry.cache_key.clone(),
            path: entry.path.clone(),
            estimated_bytes: entry.estimated_bytes.unwrap_or(0),
            last_used_at_ms: entry.last_used_at_ms.unwrap_or(0),
            age_days: entry.age_days.unwrap_or(0),
            reason: entry.reason.clone(),
        })
        .collect::<Vec<_>>();
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
    let inventory = GcGateCacheInventory {
        root: location.root.to_string_lossy().into_owned(),
        repository_key: location.repository_key.clone(),
        budget_bytes,
        total_bytes: entries
            .iter()
            .filter_map(|entry| entry.estimated_bytes)
            .fold(0_u64, u64::saturating_add),
        reclaimable_bytes: sum(GcGateCacheDisposition::Reclaimable),
        held_bytes: sum(GcGateCacheDisposition::Held),
        active_bytes: sum(GcGateCacheDisposition::Active),
        include_active,
        holders,
        entries,
    };
    (inventory, candidates)
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

/// Remove one reviewed candidate, re-proving at the moment of removal that no
/// gate is using it.
///
/// The plan may be minutes old. So the entry's own lease is *taken* -- a gate
/// that starts now waits for it rather than racing the removal -- and the
/// pidfiles and owner locks are read again under it. A live entry is then
/// renamed aside, exactly as a gate rotates an oversized cache, and the lease
/// is released before the slow part: the next gate gets a fresh directory at
/// once instead of waiting minutes for an unlink.
pub(crate) fn reclaim(
    main_root: &Path,
    candidate: &GcGateCacheCandidate,
    deadline: Option<Instant>,
) -> Result<GateCacheReclaim, String> {
    let location = location(main_root)
        .ok_or_else(|| "this repository's gate cache location cannot be resolved".to_string())?;
    reclaim_in(main_root, &location, candidate, deadline)
}

/// [`reclaim`] against an explicit location and registry.
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
    let current_bytes = directory_size_without_following_links(&path).ok();
    if current_used != Some(candidate.last_used_at_ms)
        || current_bytes != Some(candidate.estimated_bytes)
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
            estimated_bytes: directory_size_without_following_links(
                &fixture.location.root.join(entry),
            )
            .unwrap_or(0),
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
        inspect(
            &fixture.main_root,
            &fixture.location,
            KeepRule {
                budget_bytes: 0,
                include_active: false,
                configured_keys: &configured.iter().map(|key| key.to_string()).collect(),
            },
            crate::clock::epoch_ms(),
            crate::SizeScan::Measure,
            &mut records,
        )
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

        // Same size, later use: still not the reviewed entry.
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
        let (inventory, candidates) = inspect(
            &fixture.main_root,
            &fixture.location,
            KeepRule {
                budget_bytes: 0,
                include_active: false,
                configured_keys: &Default::default(),
            },
            crate::clock::epoch_ms(),
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
