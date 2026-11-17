//! Choosing and removing what goes, per class.
//!
//! Deletion sites, and why each removal is safe:
//! - `gate_cache` (`<host cache>/gates/<repository>/<key>`): a cargo target a
//!   gate rebuilds; regenerable. Never while a lease in the host resource
//!   registry names it, and never when the registry cannot be read.
//! - `recovery_archives` (`<host state>/recovery-archives/<repository>/<id>`):
//!   for size, only when every commit it records is contained in the
//!   repository's default branch and it holds no patch, untracked or ignored
//!   file (landed); otherwise only once older than the retention (expired).
//! - `symbol_index`, `graph_stores`, `tool_caches`, `qualification_retained`:
//!   indexes and caches rebuilt on demand; a child used within the class's
//!   idle time is kept.
//! - `release_artifacts`, `rollout_artifacts`, `quick_test_pools`: logs,
//!   backups and pools left by release and test runs, removed whole once
//!   nothing in them changed for their age (expired).
//!
//! Everything else is never removed here.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

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

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn child_candidate(item: &Item, child: &Child, reason: Reason) -> Candidate {
    Candidate {
        class: item.class,
        root_kind: item.root_kind,
        root: item.root.clone(),
        relative: format!("{}/{}", item.name, child.name),
        bytes: child.bytes,
        newest_ms: child.newest_ms,
        reason,
    }
}

/// Gate cache bytes per repository, from the ledger's children.
pub fn gate_cache_by_repository(ledger: &Ledger) -> BTreeMap<String, u64> {
    let mut totals = BTreeMap::new();
    for item in ledger
        .items
        .iter()
        .filter(|item| item.class == Class::GateCache)
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
    for item in &ledger.items {
        let pressing = aggressive.contains(&item.root_kind);
        match policy(item.class) {
            Policy::ReportOnly(_) => {}
            Policy::ExpireEntry { age } => {
                if item.measured_at_ms > 0
                    && item.newest_ms > 0
                    && now.saturating_sub(item.newest_ms) > duration_ms(age)
                {
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
                let mut kept: Vec<&Child> = Vec::new();
                for child in &item.children {
                    let age = now.saturating_sub(child.newest_ms);
                    if expire.is_some_and(|expire| age > duration_ms(expire)) {
                        expired.push(child_candidate(item, child, Reason::Expired));
                    } else {
                        kept.push(child);
                    }
                }
                let budget = if pressing {
                    0
                } else {
                    budgets.class_bytes(item.class).unwrap_or(u64::MAX)
                };
                let mut total = class_bytes.get(&item.class).copied().unwrap_or(0);
                kept.sort_by_key(|child| child.newest_ms);
                for child in kept {
                    if total <= budget {
                        break;
                    }
                    if now.saturating_sub(child.newest_ms) < duration_ms(idle) {
                        continue;
                    }
                    total = total.saturating_sub(child.bytes);
                    over.push(child_candidate(item, child, Reason::OverBudget));
                }
            }
            Policy::GateCache => over.extend(plan_gate_cache(item, budgets, pressing)),
            Policy::RecoveryArchives => {
                let retention = budgets
                    .recovery_archive_retention_days
                    .saturating_mul(86_400_000);
                let mut children: Vec<&Child> = item.children.iter().collect();
                children.sort_by_key(|child| child.newest_ms);
                let budget = if pressing {
                    0
                } else {
                    budgets.recovery_archives_bytes
                };
                let total = class_bytes.get(&item.class).copied().unwrap_or(0);
                for child in children {
                    if now.saturating_sub(child.newest_ms) > retention {
                        expired.push(child_candidate(item, child, Reason::Expired));
                    } else if total > budget {
                        // Each is proven landed before removal; the executor
                        // stops once the class is within budget.
                        over.push(child_candidate(item, child, Reason::OverBudget));
                    }
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
            let entry = newest_per_kind
                .entry(kind)
                .or_insert((child.newest_ms, child.name.as_str()));
            if child.newest_ms > entry.0 {
                *entry = (child.newest_ms, child.name.as_str());
            }
        }
        let keep: BTreeSet<&str> = newest_per_kind.values().map(|(_, name)| *name).collect();
        let mut evict: Vec<&&Child> = children
            .iter()
            .filter(|child| !keep.contains(child.name.as_str()))
            .collect();
        evict.sort_by_key(|child| child.newest_ms);
        candidates.extend(
            evict
                .into_iter()
                .map(|child| child_candidate(item, child, Reason::OverBudget)),
        );
    }
    candidates
}

/// Gate cache keys a live lease holds, per repository; `None` when the
/// registry cannot be read (then no gate cache entry is removed).
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

/// Why a candidate may not be removed right now, checked just before
/// removal; `None` when it may.
pub fn refuse(
    candidate: &Candidate,
    leases: Option<&BTreeSet<(String, String)>>,
) -> Option<String> {
    let relative = Path::new(&candidate.relative);
    let safe = relative
        .components()
        .all(|component| matches!(component, std::path::Component::Normal(_)));
    if !safe || candidate.relative.is_empty() {
        return Some("not a plain path inside its root".into());
    }
    let path = candidate.path();
    match fs::symlink_metadata(&path) {
        Err(_) => return Some("already gone".into()),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Some("a symbolic link".into());
        }
        Ok(_) => {}
    }
    match candidate.class {
        Class::GateCache => {
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
        Class::RecoveryArchives if candidate.reason == Reason::OverBudget => {
            if let Err(why) = archive_landed(&path) {
                return Some(format!("unlanded: {why}"));
            }
        }
        _ => {}
    }
    None
}

/// Remove a candidate: renamed aside first (so a half-removed tree is never
/// mistaken for a valid one), then deleted.
pub fn remove(candidate: &Candidate) -> std::io::Result<()> {
    let path = candidate.path();
    let aside = candidate.root.join(format!(
        ".aethyme-storage-evicting-{}-{}",
        std::process::id(),
        super::now_ms()
    ));
    fs::rename(&path, &aside)?;
    if fs::symlink_metadata(&aside)?.is_dir() {
        fs::remove_dir_all(&aside)
    } else {
        fs::remove_file(&aside)
    }
}

/// Leftovers of removals interrupted after the rename.
pub fn sweep_aside(root: &Path) {
    for name in super::read_names(root) {
        if !name.starts_with(".aethyme-storage-evicting-") {
            continue;
        }
        let path = root.join(&name);
        let removed = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        crate::warn_unrecorded("remove an interrupted storage eviction", removed);
    }
}
