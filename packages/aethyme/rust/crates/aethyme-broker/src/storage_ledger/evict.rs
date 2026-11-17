//! Choosing and removing what goes, per class.
//!
//! Deletion sites, and why each removal is safe:
//! - `gate_cache` (`<host cache>/gates/<repository>/<key>`): a cargo target a
//!   gate rebuilds; regenerable. Removed only while holding the entry's
//!   exclusive gate cache lease, so a gate never loses it mid-run.
//! - `recovery_archives` (`<host state>/recovery-archives/<repository>/<id>`):
//!   only for size, only when every commit it records is contained in the
//!   repository's default branch and it holds no patch, untracked or ignored
//!   file (landed), and only after its manifest's creation time plus its
//!   recorded minimum retention. Never by age alone; an unlanded archive is
//!   never removed here, only reported by id.
//! - `symbol_index`, `graph_stores`, `tool_caches`, `qualification_retained`:
//!   indexes and caches rebuilt on demand; a child used within the class's
//!   idle time is kept, and recency is re-read just before removal.
//! - `release_artifacts`, `rollout_artifacts`, `quick_test_pools`: leftovers
//!   of release and test runs with Aethyme's exact generated names, removed
//!   whole once nothing in them changed for their age.
//!
//! Every removal first renames the entry aside (atomically, out of its live
//! location), then deletes the renamed copy without following links. A path
//! with a symbolic link anywhere below its root is never removed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Budgets, Child, Class, Item, Ledger, Policy, RootKind, policy};

/// Why something is proposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Older than its class allows.
    Expired,
    /// Its class or root is over budget, or a volume is below its floor.
    OverBudget,
}

/// One proposed removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub class: Class,
    pub root_kind: RootKind,
    /// The entry's root, and the path relative to it.
    pub root: PathBuf,
    pub relative: String,
    pub bytes: u64,
    /// The most recent change the measurement saw (0 when unknown).
    pub newest_ms: u64,
    pub reason: Reason,
}

impl Candidate {
    pub fn path(&self) -> PathBuf {
        self.root.join(&self.relative)
    }

    /// How it is named in reports: class and path inside Aethyme's root.
    pub fn label(&self) -> String {
        format!("{}:{}", self.class.as_str(), self.relative)
    }
}

pub(crate) fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn child_candidate(item: &Item, child: &Child, reason: Reason) -> Candidate {
    Candidate {
        class: item.class,
        root_kind: item.root_kind,
        root: item.root.clone(),
        relative: format!("{}/{}", item.name, child.name),
        bytes: child.bytes,
        newest_ms: child.newest_ms.max(child.ctime_ms),
        reason,
    }
}

/// When a child was last used: its newest change, else its status-change
/// time; `None` when neither is known (then it is kept).
fn last_used(child: &Child) -> Option<u64> {
    [child.newest_ms, child.ctime_ms]
        .into_iter()
        .find(|ms| *ms > 0)
}

/// Gate cache bytes per repository, from the ledger's children.
pub fn gate_cache_by_repository(ledger: &Ledger) -> BTreeMap<String, u64> {
    let mut totals = BTreeMap::new();
    for item in ledger
        .items
        .iter()
        .filter(|item| item.class == Class::GateCache && item.measured)
    {
        for child in &item.children {
            if let Some((repository, _)) = child.name.split_once('/') {
                *totals.entry(repository.to_string()).or_insert(0) += child.bytes;
            }
        }
    }
    totals
}

/// What a pass may remove, in removal order. `aggressive` (a root over its
/// total or a volume below its floor) evicts regenerable children down to
/// nothing, still keeping every child used within its idle time, the most
/// recently used gate cache per kind and every unlanded archive.
pub fn plan(
    ledger: &Ledger,
    budgets: &Budgets,
    now: u64,
    aggressive: &BTreeSet<RootKind>,
) -> Vec<Candidate> {
    let mut expired = Vec::new();
    let mut over = Vec::new();
    let class_bytes = ledger.class_bytes();
    for item in ledger.items.iter().filter(|item| item.measured) {
        let pressing = aggressive.contains(&item.root_kind);
        match policy(item.class) {
            Policy::ReportOnly(_) => {}
            Policy::ExpireEntry { age } => {
                if item.newest_ms > 0 && now.saturating_sub(item.newest_ms) > duration_ms(age) {
                    expired.push(Candidate {
                        class: item.class,
                        root_kind: item.root_kind,
                        root: item.root.clone(),
                        relative: item.name.clone(),
                        bytes: item.bytes,
                        newest_ms: item.newest_ms,
                        reason: Reason::Expired,
                    });
                }
            }
            Policy::LruChildren { idle, expire } => {
                let mut idle_children: Vec<(&Child, u64)> = Vec::new();
                for child in &item.children {
                    // Unknown recency is not "oldest": such a child is kept.
                    let Some(used) = last_used(child) else {
                        continue;
                    };
                    let age = now.saturating_sub(used);
                    if expire.is_some_and(|expire| age > duration_ms(expire)) {
                        expired.push(child_candidate(item, child, Reason::Expired));
                    } else if age >= duration_ms(idle) {
                        idle_children.push((child, used));
                    }
                }
                let budget = if pressing {
                    0
                } else {
                    budgets.class_bytes(item.class).unwrap_or(u64::MAX)
                };
                if class_bytes.get(&item.class).copied().unwrap_or(0) > budget {
                    idle_children.sort_by_key(|(_, used)| *used);
                    over.extend(
                        idle_children
                            .into_iter()
                            .map(|(child, _)| child_candidate(item, child, Reason::OverBudget)),
                    );
                }
            }
            Policy::GateCache => over.extend(plan_gate_cache(item, budgets, pressing)),
            Policy::RecoveryArchives => {
                let budget = if pressing {
                    0
                } else {
                    budgets.recovery_archives_bytes
                };
                if class_bytes.get(&item.class).copied().unwrap_or(0) > budget {
                    // Oldest by manifest creation; one without it is kept.
                    let mut children: Vec<(&Child, u64)> = item
                        .children
                        .iter()
                        .filter_map(|child| child.created_ms.map(|created| (child, created)))
                        .collect();
                    children.sort_by_key(|(_, created)| *created);
                    over.extend(
                        children
                            .into_iter()
                            .map(|(child, _)| child_candidate(item, child, Reason::OverBudget)),
                    );
                }
            }
        }
    }
    expired.sort_by_key(|candidate| candidate.newest_ms);
    expired.extend(over);
    expired
}

/// Over budget, all but the most recently used entry per gate kind.
fn plan_gate_cache(item: &Item, budgets: &Budgets, pressing: bool) -> Vec<Candidate> {
    let mut by_repository: BTreeMap<&str, Vec<&Child>> = BTreeMap::new();
    for child in &item.children {
        if let Some((repository, _)) = child.name.split_once('/') {
            by_repository.entry(repository).or_default().push(child);
        }
    }
    let mut candidates = Vec::new();
    for (repository, children) in &by_repository {
        let total: u64 = children.iter().map(|child| child.bytes).sum();
        if !pressing && total <= budgets.gate_budget(repository) {
            continue;
        }
        let mut newest_per_kind: BTreeMap<&str, (u64, &str)> = BTreeMap::new();
        for child in children {
            let key = child.name.split_once('/').map_or("", |(_, key)| key);
            let kind = crate::gate_cache_gc::cache_kind(key);
            // Unknown recency counts as most recent: it is kept.
            let used = last_used(child).unwrap_or(u64::MAX);
            let entry = newest_per_kind
                .entry(kind)
                .or_insert((used, child.name.as_str()));
            if used > entry.0 {
                *entry = (used, child.name.as_str());
            }
        }
        let keep: BTreeSet<&str> = newest_per_kind.values().map(|(_, name)| *name).collect();
        let mut evict: Vec<&&Child> = children
            .iter()
            .filter(|child| !keep.contains(child.name.as_str()) && last_used(child).is_some())
            .collect();
        evict.sort_by_key(|child| last_used(child).unwrap_or(u64::MAX));
        candidates.extend(
            evict
                .into_iter()
                .map(|child| child_candidate(item, child, Reason::OverBudget)),
        );
    }
    candidates
}

/// Gate cache keys a live lease holds, per repository; `None` when the
/// registry cannot be read.
pub fn leased_gate_caches(registry: &Path) -> Option<BTreeSet<(String, String)>> {
    if !registry.is_file() {
        return None;
    }
    let coordinator = crate::HostResourceCoordinator::open_read_only(registry).ok()?;
    let leases = coordinator.list(false).ok()?;
    let prefix = crate::gates::managed_cache_lease_name("", "");
    let mut held = BTreeSet::new();
    for lease in leases {
        for allocation in &lease.allocations {
            if allocation.kind != "exclusive_key" {
                continue;
            }
            // `aethyme-gate-cache:<repository>:<key>`.
            if let Some(rest) = allocation.value.strip_prefix(prefix.trim_end_matches(':'))
                && let Some((repository, key)) = rest.trim_start_matches(':').split_once(':')
            {
                held.insert((repository.to_string(), key.to_string()));
            }
        }
    }
    Some(held)
}

/// Whether an archive's content is proven landed: no uncommitted content,
/// and every commit it records is in its repository's default branch.
pub fn archive_landed(archive: &Path) -> Result<(), String> {
    let manifest_path = archive.join("manifest.json");
    let size = fs::metadata(&manifest_path)
        .map_err(|error| format!("manifest unreadable: {error}"))?
        .len();
    if size > 32 * 1024 * 1024 {
        return Err("manifest too large".into());
    }
    let manifest: Value = serde_json::from_slice(
        &fs::read(&manifest_path).map_err(|error| format!("manifest unreadable: {error}"))?,
    )
    .map_err(|error| format!("manifest invalid: {error}"))?;
    for patch in ["staged_patch", "unstaged_patch"] {
        if manifest
            .pointer(&format!("/{patch}/bytes"))
            .and_then(Value::as_u64)
            != Some(0)
        {
            return Err(format!("it holds a {patch} or does not say"));
        }
    }
    for files in ["untracked_files", "ignored_files"] {
        if !manifest
            .get(files)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            return Err(format!("it holds {files} or does not say"));
        }
    }
    let repository = manifest
        .get("repository")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .ok_or("its repository is gone")?;
    let mut commits: Vec<String> = ["head", "branch_tip", "checkout_branch_tip"]
        .iter()
        .filter_map(|field| manifest.get(*field).and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    commits.extend(
        manifest
            .get("unlanded_commits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string),
    );
    if commits.is_empty() {
        return Err("it records no commit".into());
    }
    let default = default_branch(&repository).ok_or("no default branch")?;
    for commit in commits {
        if !commit.chars().all(|c| c.is_ascii_hexdigit()) || commit.len() < 40 {
            return Err("an invalid commit id".into());
        }
        let contained = git(
            &repository,
            &["merge-base", "--is-ancestor", &commit, &default],
        )
        .is_some();
        if !contained {
            return Err(format!("commit {} is not in {default}", &commit[..12]));
        }
    }
    Ok(())
}

fn git(repository: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The commit the repository's default branch points at, preferring the
/// remote's view.
fn default_branch(repository: &Path) -> Option<String> {
    let remote_head = git(
        repository,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    );
    let mut names: Vec<String> = remote_head.into_iter().collect();
    names.extend(
        [
            "refs/remotes/origin/main",
            "refs/remotes/origin/master",
            "refs/heads/main",
            "refs/heads/master",
        ]
        .map(str::to_string),
    );
    names.into_iter().find_map(|name| {
        git(
            repository,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{name}^{{commit}}"),
            ],
        )
    })
}

/// Whether `relative` under `root` is a plain path with no symbolic link in
/// it (the root itself is trusted: it is an exact configured path).
pub fn link_free(root: &Path, relative: &str) -> bool {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return false;
    }
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.file_type().is_symlink() => {}
            _ => return false,
        }
    }
    true
}

/// The newest modification of an entry's top two levels (bounded), read
/// just before removal.
fn recent_change(path: &Path) -> u64 {
    let mut newest = fs::symlink_metadata(path).map_or(0, |metadata| super::mtime_ms(&metadata));
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten().take(1000) {
            if let Ok(metadata) = fs::symlink_metadata(entry.path()) {
                newest = newest.max(super::mtime_ms(&metadata));
            }
        }
    }
    newest
}

/// Why a candidate may not be removed right now, checked just before
/// removal; `None` when it may.
pub fn refuse(
    candidate: &Candidate,
    leases: Option<&BTreeSet<(String, String)>>,
    now: u64,
) -> Option<String> {
    if !link_free(&candidate.root, &candidate.relative) {
        return Some("a symbolic link, or not a plain path inside its root".into());
    }
    let path = candidate.path();
    match policy(candidate.class) {
        Policy::ReportOnly(why) => return Some(why.into()),
        Policy::LruChildren { idle, .. } => {
            let changed = recent_change(&path);
            if changed > candidate.newest_ms || now.saturating_sub(changed) < duration_ms(idle) {
                return Some("used since it was measured".into());
            }
        }
        Policy::ExpireEntry { age } => {
            let changed = recent_change(&path);
            if changed > candidate.newest_ms || now.saturating_sub(changed) <= duration_ms(age) {
                return Some("changed since it was measured".into());
            }
        }
        Policy::GateCache => {
            let mut parts = candidate.relative.splitn(3, '/');
            let (Some(_), Some(repository), Some(key)) = (parts.next(), parts.next(), parts.next())
            else {
                return Some("not a gate cache entry".into());
            };
            match leases {
                None => return Some("the lease registry cannot be read".into()),
                Some(held) if held.contains(&(repository.to_string(), key.to_string())) => {
                    return Some("a live lease holds it".into());
                }
                Some(_) => {}
            }
        }
        Policy::RecoveryArchives => {
            if let Err(why) = archive_landed(&path) {
                return Some(format!("unlanded: {why}"));
            }
            let (created, retention) = super::archive_times(&path);
            let retention =
                retention.unwrap_or(crate::retention::MIN_RECOVERY_ARCHIVE_RETENTION_MS);
            match created {
                Some(created) if now >= created.saturating_add(retention) => {}
                _ => return Some("within its promised minimum retention".into()),
            }
        }
    }
    None
}

/// Remove a candidate under its class's protection against a live user:
/// - a gate cache entry: the gate cache's exclusive lease
///   (`managed_cache_lease_name`) is acquired first, held across the rename
///   and the deletion, and released on every path; if a gate holds it, the
///   entry stays;
/// - every other removable class has no lease: the entry is renamed out of
///   its live location atomically first, so a process that opens it after
///   the rename finds it absent (and rebuilds), never half deleted.
///
/// `Err` says why it stayed.
pub fn remove_protected(
    candidate: &Candidate,
    registry: &Path,
    beat: &mut dyn FnMut(),
) -> Result<(), String> {
    if candidate.class != Class::GateCache {
        return remove(candidate, beat).map_err(|error| error.to_string());
    }
    let mut parts = candidate.relative.splitn(3, '/');
    let (Some(_), Some(repository), Some(key)) = (parts.next(), parts.next(), parts.next()) else {
        return Err("not a gate cache entry".into());
    };
    // `open` would create a missing registry: a lease there proves nothing.
    if !registry.is_file() {
        return Err("the lease registry cannot be read".into());
    }
    let mut coordinator =
        crate::HostResourceCoordinator::open(registry).map_err(|error| error.to_string())?;
    let request = crate::HostResourceRequest {
        schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
        request_id: format!(
            "storage-gate-cache-{}-{}-{key}",
            std::process::id(),
            super::now_ms()
        ),
        repository: repository.to_string(),
        worktree_fingerprint: "storage".into(),
        run_id: format!("storage-{}", std::process::id()),
        ttl_seconds: 300,
        holder_pid: Some(std::process::id()),
        resources: vec![crate::HostResourceRequirement {
            key: "managed_cache".into(),
            resource: crate::HostResourceKind::ExclusiveKey {
                name: crate::gates::managed_cache_lease_name(repository, key),
            },
        }],
    };
    let grant = coordinator
        .acquire(&request)
        .map_err(|error| format!("a gate holds it ({error})"))?;
    let removed = remove(candidate, beat).map_err(|error| error.to_string());
    crate::warn_unrecorded(
        "release the gate cache lease storage eviction took",
        coordinator.release(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        ),
    );
    removed
}

/// Remove a candidate: renamed aside first (so a half-removed tree is never
/// mistaken for a valid one), then deleted without following links.
pub fn remove(candidate: &Candidate, beat: &mut dyn FnMut()) -> std::io::Result<()> {
    let path = candidate.path();
    let aside = candidate.root.join(format!(
        ".aethyme-storage-evicting-{}-{}",
        std::process::id(),
        super::now_ms()
    ));
    fs::rename(&path, &aside)?;
    delete_tree(&aside, beat)
}

/// Delete a tree bottom up, never following a link, refreshing `beat` at
/// least every second.
pub fn delete_tree(path: &Path, beat: &mut dyn FnMut()) -> std::io::Result<()> {
    let mut last_beat = Instant::now();
    let mut stack = vec![(path.to_path_buf(), false)];
    while let Some((next, visited)) = stack.pop() {
        let metadata = match fs::symlink_metadata(&next) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.is_dir() {
            if visited {
                fs::remove_dir(&next)?;
            } else {
                stack.push((next.clone(), true));
                for entry in fs::read_dir(&next)?.flatten() {
                    stack.push((entry.path(), false));
                }
            }
        } else {
            // A file or a link: the link itself goes, never its target.
            fs::remove_file(&next)?;
        }
        if last_beat.elapsed() >= super::BEAT_EVERY {
            beat();
            last_beat = Instant::now();
        }
    }
    Ok(())
}

/// Leftovers of removals interrupted after the rename.
pub fn sweep_aside(root: &Path, beat: &mut dyn FnMut()) {
    for name in super::read_names(root) {
        if !name.starts_with(".aethyme-storage-evicting-") {
            continue;
        }
        crate::warn_unrecorded(
            "remove an interrupted storage eviction",
            delete_tree(&root.join(&name), beat),
        );
    }
}
