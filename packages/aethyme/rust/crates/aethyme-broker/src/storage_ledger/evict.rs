//! Choosing and removing what goes, per class.
//!
//! Deletion sites, and why each removal is safe:
//! - `gate_cache` (`<host cache>/gates/<repository>/<key>`): a cargo target a
//!   gate rebuilds; regenerable. Superseded workspace build units are pruned
//!   out of it first ([`super::gate_prune`]); a whole entry is removed only
//!   while holding the entry's exclusive gate cache lease, so a gate never
//!   loses it mid-run.
//! - `recovery_archives` (`<host state>/recovery-archives/<repository>/<id>`):
//!   only for size, only when every commit it records is contained in the
//!   repository's default branch and it holds no patch, untracked or ignored
//!   file (landed), and only after its manifest's creation time plus its
//!   recorded minimum retention. Never by age alone; an unlanded archive is
//!   never removed here, only reported by id.
//! - `symbol_index`, `graph_stores`, `tool_caches`, `qualification_retained`:
//!   indexes and caches rebuilt on demand; a child used within the class's
//!   idle time is kept, and recency is re-read just before removal. The
//!   Node.js compile cache's past days go an hour after their last change;
//!   today's directory is never removed.
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
use std::time::{Duration, Instant};

use serde_json::Value;

use super::landing::Refusal;
use super::{Budgets, Child, Class, Item, Ledger, Policy, RootKind};

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
    /// Recovery archives expiring by age: when they expire.
    pub expires_ms: Option<u64>,
    /// Recovery archives expiring by age: `[budget] expire_unlanded_archives`
    /// lets one holding unlanded work go too.
    pub allow_unlanded: bool,
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

pub(super) fn child_candidate(item: &Item, child: &Child, reason: Reason) -> Candidate {
    Candidate {
        class: item.class,
        root_kind: item.root_kind,
        root: item.root.clone(),
        relative: format!("{}/{}", item.name, child.name),
        bytes: child.bytes,
        newest_ms: child.newest_ms.max(child.ctime_ms),
        reason,
        expires_ms: None,
        allow_unlanded: false,
    }
}

/// When a child was last used: its newest change, else its status-change
/// time; `None` when neither is known (then it is kept).
fn last_used(child: &Child) -> Option<u64> {
    [child.newest_ms, child.ctime_ms]
        .into_iter()
        .find(|ms| *ms > 0)
}

/// How the children of an entry evicted child by child are judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ChildRule {
    pub(super) idle: Duration,
    pub(super) expire: Option<Duration>,
    /// Day-named children ([`Policy::DailyChildren`]): a past day's goes once
    /// unchanged this long; today's never goes.
    pub(super) past_day_idle: Option<Duration>,
}

impl ChildRule {
    /// The rule of an entry's policy, if it is evicted child by child.
    pub(super) fn of(policy: Policy) -> Option<Self> {
        match policy {
            Policy::LruChildren { idle, expire } => Some(Self {
                idle,
                expire,
                past_day_idle: None,
            }),
            Policy::DailyChildren {
                idle,
                expire,
                past_day_idle,
            } => Some(Self {
                idle,
                expire: Some(expire),
                past_day_idle: Some(past_day_idle),
            }),
            _ => None,
        }
    }

    /// How long `child` must be unchanged before it may go, or why it may
    /// never go now.
    pub(super) fn idle_for(&self, child: &str, now: u64) -> Result<Duration, &'static str> {
        match self.past_day_idle.map(|past_day_idle| {
            (
                past_day_idle,
                crate::node_compile_cache::past_day(child, now),
            )
        }) {
            Some((_, Some(false))) => Err("today's directory, still in use"),
            Some((past_day_idle, Some(true))) => Ok(past_day_idle),
            _ => Ok(self.idle),
        }
    }
}

/// What the plan does with one child of an entry evicted child by child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChildFate {
    Keep,
    /// Goes whatever the budget.
    Expired,
    /// Goes while its class is over budget, least recently used first.
    Idle {
        used: u64,
    },
}

/// The fate of `child` under `rule` at `now`. Unknown recency is not
/// "oldest": such a child is kept. A past day's child goes once unchanged
/// for its idle time and is never budget-evicted before; today's is kept.
pub(super) fn child_fate(child: &Child, rule: ChildRule, now: u64) -> ChildFate {
    let Some(used) = last_used(child) else {
        return ChildFate::Keep;
    };
    let age = now.saturating_sub(used);
    if rule.past_day_idle.is_some()
        && crate::node_compile_cache::past_day(&child.name, now).is_some()
    {
        return match rule.idle_for(&child.name, now) {
            Ok(idle) if age >= duration_ms(idle) => ChildFate::Expired,
            _ => ChildFate::Keep,
        };
    }
    if rule.expire.is_some_and(|expire| age > duration_ms(expire)) {
        ChildFate::Expired
    } else if age >= duration_ms(rule.idle) {
        ChildFate::Idle { used }
    } else {
        ChildFate::Keep
    }
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
    let mut archives_expired = Vec::new();
    let mut over = Vec::new();
    let class_bytes = ledger.class_bytes();
    for item in ledger.items.iter().filter(|item| item.measured) {
        let pressing = aggressive.contains(&item.root_kind);
        match super::entry_policy(item.class, &item.name) {
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
                        expires_ms: None,
                        allow_unlanded: false,
                    });
                }
            }
            policy @ (Policy::LruChildren { .. } | Policy::DailyChildren { .. }) => {
                let Some(rule) = ChildRule::of(policy) else {
                    continue;
                };
                let mut idle_children: Vec<(&Child, u64)> = Vec::new();
                for child in &item.children {
                    match child_fate(child, rule, now) {
                        ChildFate::Keep => {}
                        ChildFate::Expired => {
                            expired.push(child_candidate(item, child, Reason::Expired));
                        }
                        ChildFate::Idle { used } => idle_children.push((child, used)),
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
            Policy::GateCache => {
                // Entries unused for the artifact max age go whatever the
                // budget; the rest are judged for budget.
                expired.extend(
                    item.children
                        .iter()
                        .filter(|child| gate_entry_expired(child, budgets, now))
                        .map(|child| child_candidate(item, child, Reason::Expired)),
                );
                over.extend(plan_gate_cache(item, budgets, pressing, now));
            }
            Policy::RecoveryArchives => {
                // Pressure does not empty this budget: below a volume's
                // floor, the archive floor removes the oldest removable
                // archives only until free space is back at it, paced
                // (`archive_floor`), instead of every landed one at once.
                let budget = budgets.recovery_archives_bytes;
                let over_budget = class_bytes.get(&item.class).copied().unwrap_or(0) > budget;
                // Oldest by manifest creation; one without it is kept.
                let mut children: Vec<(&Child, u64)> = item
                    .children
                    .iter()
                    .filter_map(|child| child.created_ms.map(|created| (child, created)))
                    .collect();
                children.sort_by_key(|(_, created)| *created);
                for (child, created) in children {
                    let notice = ledger.archive_notices.get(&child.name);
                    let deletable =
                        !ledger.age_frozen && super::archive_deletable(child, budgets, notice, now);
                    match super::archive_expiry(child, budgets) {
                        // An age rule: whatever the budget, landed or not,
                        // once its notice was served.
                        Some(expires) if deletable => {
                            let mut candidate = child_candidate(item, child, Reason::Expired);
                            candidate.newest_ms = created;
                            candidate.expires_ms = Some(expires);
                            candidate.allow_unlanded = budgets.expire_unlanded_archives;
                            archives_expired.push(candidate);
                        }
                        _ if over_budget => {
                            over.push(child_candidate(item, child, Reason::OverBudget));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    expired.sort_by_key(|candidate| candidate.newest_ms);
    // Archives expire oldest first (they were sorted by creation).
    expired.extend(archives_expired);
    expired.extend(over);
    expired
}

/// Whether a gate cache entry (`<repository>/<key>`) was unused for the
/// artifact max age: its newest change (every gate rebuilds the workspace
/// members it tests into it, #391) is that old. Unknown recency, or the
/// max age off, is not expired.
pub fn gate_entry_expired(child: &Child, budgets: &Budgets, now: u64) -> bool {
    budgets.artifact_max_age().is_some_and(|max_age| {
        last_used(child).is_some_and(|used| now.saturating_sub(used) >= duration_ms(max_age))
    })
}

/// How long the most recently used entry of a gate kind must be unused
/// before it goes for budget (outside pressure): its next gate rebuilds from
/// nothing, so an entry in daily use is kept.
pub const GATE_CACHE_LAST_ENTRY_IDLE: Duration = Duration::from_secs(24 * 60 * 60);

/// Over budget, all but the most recently used entry per gate kind, least
/// recently used first; then, still least recently used first, the most
/// recently used entries of the other kinds unused for
/// [`GATE_CACHE_LAST_ENTRY_IDLE`] (any time under pressure). The
/// repository's most recently used entry, and one of unknown recency, are
/// always kept. The pass stops removing once the repository is within budget.
fn plan_gate_cache(item: &Item, budgets: &Budgets, pressing: bool, now: u64) -> Vec<Candidate> {
    let mut by_repository: BTreeMap<&str, Vec<&Child>> = BTreeMap::new();
    for child in &item.children {
        if gate_entry_expired(child, budgets, now) {
            continue;
        }
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
        // Last resort: whole entries of other kinds, never the most
        // recently used one of the repository.
        let newest = children
            .iter()
            .max_by_key(|child| last_used(child).unwrap_or(u64::MAX))
            .map(|child| child.name.as_str());
        let mut last: Vec<(&Child, u64)> = children
            .iter()
            .filter(|child| {
                keep.contains(child.name.as_str()) && Some(child.name.as_str()) != newest
            })
            .filter_map(|child| last_used(child).map(|used| (*child, used)))
            .filter(|(_, used)| {
                pressing || now.saturating_sub(*used) >= duration_ms(GATE_CACHE_LAST_ENTRY_IDLE)
            })
            .collect();
        last.sort_by_key(|(_, used)| *used);
        candidates.extend(
            last.into_iter()
                .map(|(child, _)| child_candidate(item, child, Reason::OverBudget)),
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
/// Commits are proven by this process's shared [`super::landing`] prover.
pub fn archive_landed(archive: &Path) -> Result<(), String> {
    archive_landing(archive).map_err(|refusal| refusal.to_string())
}

/// [`archive_landed`], saying whether a refusal is an answer (not landed)
/// or the lack of one (unproven).
pub fn archive_landing(archive: &Path) -> Result<(), Refusal> {
    archive_landing_with(archive, &mut super::landing::shared())
}

/// [`archive_landed`] with an explicit prover.
pub fn archive_landed_with(
    archive: &Path,
    prover: &mut super::landing::LandingProver,
) -> Result<(), String> {
    archive_landing_with(archive, prover).map_err(|refusal| refusal.to_string())
}

/// [`archive_landing`] with an explicit prover.
pub fn archive_landing_with(
    archive: &Path,
    prover: &mut super::landing::LandingProver,
) -> Result<(), Refusal> {
    read_manifest(archive)
        .map_err(Refusal::NotLanded)
        .and_then(|manifest| manifest_landing(&manifest, prover))
}

/// An archive's landing (as [`archive_landed`]) and what its manifest
/// records it holds, from one read of the manifest. An unreadable manifest
/// holds [`ArchiveHolds::unreadable`].
pub fn archive_landing_and_holds(archive: &Path) -> (Result<(), String>, ArchiveHolds) {
    match read_manifest(archive) {
        Err(why) => (Err(why), ArchiveHolds::unreadable()),
        Ok(manifest) => (
            manifest_landing(&manifest, &mut super::landing::shared())
                .map_err(|refusal| refusal.to_string()),
            ArchiveHolds::from_manifest(&manifest),
        ),
    }
}

fn manifest_landing(
    manifest: &Value,
    prover: &mut super::landing::LandingProver,
) -> Result<(), Refusal> {
    content_and_commits(manifest)
        .map_err(Refusal::NotLanded)
        .and_then(|(repository, commits)| prover.prove(&repository, &commits))
}

/// An archive's parsed `manifest.json` (at most 32 MiB).
fn read_manifest(archive: &Path) -> Result<Value, String> {
    let manifest_path = archive.join("manifest.json");
    let size = fs::metadata(&manifest_path)
        .map_err(|error| format!("manifest unreadable: {error}"))?
        .len();
    if size > 32 * 1024 * 1024 {
        return Err("manifest too large".into());
    }
    serde_json::from_slice(
        &fs::read(&manifest_path).map_err(|error| format!("manifest unreadable: {error}"))?,
    )
    .map_err(|error| format!("manifest invalid: {error}"))
}

/// What a recovery archive's manifest records it holds. A `None` count is a
/// field the manifest does not carry: manifests before schema 5 have no
/// `ignored_files`, and archives made outside the broker may carry none of
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ArchiveHolds {
    /// Commits the archive's bundle carries that were not landed when it was
    /// written (`unlanded_commits`); whether they landed since is proven
    /// separately.
    pub unlanded_commits: Option<u64>,
    /// `staged_patch.bytes`.
    pub staged_bytes: Option<u64>,
    /// `unstaged_patch.bytes`.
    pub unstaged_bytes: Option<u64>,
    /// Entries of `untracked_files`.
    pub untracked_files: Option<u64>,
    /// Entries of `ignored_files`.
    pub ignored_files: Option<u64>,
    /// The manifest lacks a field that says whether the archive holds work
    /// outside its commits (a patch, untracked or ignored files), or cannot
    /// be read: such an archive is never called landed.
    pub unknown: bool,
    /// The manifest's `schema_version`, named when it does not say.
    #[serde(skip)]
    schema_version: Option<u64>,
}

impl ArchiveHolds {
    /// What `manifest` records, field by field, across every schema.
    pub fn from_manifest(manifest: &Value) -> Self {
        let count = |field: &str| {
            manifest.get(field).and_then(|value| {
                value
                    .as_array()
                    .map(|items| items.len() as u64)
                    .or_else(|| value.as_u64())
            })
        };
        let list = |field: &str| {
            manifest
                .get(field)
                .and_then(Value::as_array)
                .map(|items| items.len() as u64)
        };
        let bytes = |field: &str| {
            manifest
                .pointer(&format!("/{field}/bytes"))
                .and_then(Value::as_u64)
        };
        let mut holds = Self {
            unlanded_commits: count("unlanded_commits"),
            staged_bytes: bytes("staged_patch"),
            unstaged_bytes: bytes("unstaged_patch"),
            untracked_files: list("untracked_files"),
            ignored_files: list("ignored_files"),
            unknown: false,
            schema_version: manifest.get("schema_version").and_then(Value::as_u64),
        };
        holds.unknown = !holds.missing().is_empty();
        holds
    }

    /// An archive whose manifest cannot be read: nothing known.
    pub fn unreadable() -> Self {
        Self {
            unknown: true,
            ..Self::default()
        }
    }

    /// The manifest fields that decide landing and are absent.
    fn missing(&self) -> Vec<&'static str> {
        [
            ("staged_patch", self.staged_bytes),
            ("unstaged_patch", self.unstaged_bytes),
            ("untracked_files", self.untracked_files),
            ("ignored_files", self.ignored_files),
        ]
        .into_iter()
        .filter(|(_, value)| value.is_none())
        .map(|(field, _)| field)
        .collect()
    }

    /// Why the archive cannot be landed whatever Git holds: what it records
    /// outside its commits, then what its manifest does not say, led by its
    /// unlanded commits so the most valuable content is named first. `None`
    /// when it records nothing outside its commits (they are then proven).
    pub fn reason(&self) -> Option<String> {
        let positive = |value: Option<u64>| value.filter(|value| *value > 0);
        let outside = [
            self.staged_bytes,
            self.unstaged_bytes,
            self.untracked_files,
            self.ignored_files,
        ]
        .into_iter()
        .any(|value| positive(value).is_some());
        let missing = self.missing();
        if !outside && missing.is_empty() {
            return None;
        }
        let plural = |count: u64, noun: &str| {
            if count == 1 {
                format!("1 {noun}")
            } else {
                format!("{count} {noun}s")
            }
        };
        let mut parts = Vec::new();
        if let Some(count) = positive(self.unlanded_commits) {
            parts.push(plural(count, "unlanded commit"));
        }
        if let Some(bytes) = positive(self.staged_bytes) {
            parts.push(format!("a {bytes}-byte staged patch"));
        }
        if let Some(bytes) = positive(self.unstaged_bytes) {
            parts.push(format!("a {bytes}-byte unstaged patch"));
        }
        if let Some(count) = positive(self.untracked_files) {
            parts.push(plural(count, "untracked file"));
        }
        if let Some(count) = positive(self.ignored_files) {
            parts.push(plural(count, "ignored file"));
        }
        let mut reason = String::new();
        if !parts.is_empty() {
            reason = format!("it holds {}", parts.join(", "));
        }
        if !missing.is_empty() {
            let schema = self.schema_version.map_or_else(
                || "no schema version".to_string(),
                |v| format!("schema {v}"),
            );
            if !reason.is_empty() {
                reason.push_str("; ");
            }
            reason.push_str(&format!(
                "its manifest ({schema}) does not say whether it holds {}",
                missing.join(", ")
            ));
        }
        Some(reason)
    }
}

/// The archive's repository and recorded commits, once its manifest says it
/// holds nothing outside them.
fn content_and_commits(manifest: &Value) -> Result<(PathBuf, Vec<String>), String> {
    if let Some(why) = ArchiveHolds::from_manifest(manifest).reason() {
        return Err(why);
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
    if commits
        .iter()
        .any(|commit| !commit.chars().all(|c| c.is_ascii_hexdigit()) || commit.len() < 40)
    {
        return Err("an invalid commit id".into());
    }
    Ok((repository, commits))
}

/// How [`refuse`] begins the reason for an archive whose landing could not
/// be proven either way (never counted as unlanded).
pub const UNPROVEN_PREFIX: &str = "landing not proven";

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
    let (entry, child) = candidate
        .relative
        .split_once('/')
        .unwrap_or((candidate.relative.as_str(), ""));
    match super::entry_policy(candidate.class, entry) {
        Policy::ReportOnly(why) => return Some(why.into()),
        policy @ (Policy::LruChildren { .. } | Policy::DailyChildren { .. }) => {
            let idle = match ChildRule::of(policy).map(|rule| rule.idle_for(child, now)) {
                Some(Ok(idle)) => idle,
                Some(Err(why)) => return Some(why.into()),
                None => return Some("not evicted child by child".into()),
            };
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
            // Expired by the artifact max age: a gate that wrote into it
            // since it was measured used it.
            if candidate.reason == Reason::Expired && recent_change(&path) > candidate.newest_ms {
                return Some("used since it was measured".into());
            }
        }
        Policy::RecoveryArchives if candidate.reason == Reason::Expired => {
            // Re-read just before removal: the keep marker, and the age from
            // the manifest only.
            if super::keep_marker(&path).exists() {
                return Some("kept by the owner's marker".into());
            }
            // Age alone never destroys work that exists nowhere else: an
            // archive holding unlanded commits or uncommitted changes is kept
            // (and named in advice) until its owner dismisses it or the
            // setting allows it.
            if !candidate.allow_unlanded
                && !super::dismiss_marker(&path).exists()
                && let Err(refusal) = archive_landing(&path)
            {
                return Some(match refusal {
                    Refusal::NotLanded(why) => {
                        crate::phase_telemetry::add(
                            "storage_archive_kept_unlanded_at_age",
                            std::time::Duration::ZERO,
                        );
                        format!(
                            "unlanded at its retention age, kept until its owner dismisses it: {why}"
                        )
                    }
                    Refusal::Unproven(why) => {
                        format!("{UNPROVEN_PREFIX} at its retention age, kept: {why}")
                    }
                });
            }
            let meta = super::archive_meta(&path);
            let Some(created) = meta.created_ms else {
                return Some("its manifest has no creation time".into());
            };
            // The plan's expiry (configured age and notice) and, re-read
            // from the manifest, its promised minimum retention.
            let promised = created.saturating_add(
                meta.min_retention_ms
                    .unwrap_or(crate::retention::MIN_RECOVERY_ARCHIVE_RETENTION_MS),
            );
            let due = candidate.expires_ms.map(|expires| expires.max(promised));
            if due.is_none_or(|due| now < due) {
                return Some("not expired yet".into());
            }
        }
        Policy::RecoveryArchives => {
            match archive_landing(&path) {
                Ok(()) => {}
                Err(Refusal::NotLanded(why)) => return Some(format!("unlanded: {why}")),
                Err(Refusal::Unproven(why)) => return Some(format!("{UNPROVEN_PREFIX}: {why}")),
            }
            let meta = super::archive_meta(&path);
            let retention = meta
                .min_retention_ms
                .unwrap_or(crate::retention::MIN_RECOVERY_ARCHIVE_RETENTION_MS);
            match meta.created_ms {
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
/// `Ok(false)` when `deadline` interrupted the deletion after the rename:
/// the remainder is left aside for the worker's sweep. `Err` says why it
/// stayed.
pub fn remove_protected(
    candidate: &Candidate,
    registry: &Path,
    beat: &mut dyn FnMut(),
    deadline: Option<Instant>,
) -> Result<bool, String> {
    if candidate.class != Class::GateCache {
        return remove(candidate, beat, deadline).map_err(|error| error.to_string());
    }
    let mut parts = candidate.relative.splitn(3, '/');
    let (Some(_), Some(repository), Some(key)) = (parts.next(), parts.next(), parts.next()) else {
        return Err("not a gate cache entry".into());
    };
    with_gate_cache_lease(registry, repository, key, || {
        remove(candidate, beat, deadline).map_err(|error| error.to_string())
    })?
}

/// Run `work` while holding the gate cache `<repository>/<key>`'s exclusive
/// lease (`managed_cache_lease_name`), released on every path. `Err` when
/// the registry cannot be read or a gate holds the lease: `work` did not run.
pub fn with_gate_cache_lease<T>(
    registry: &Path,
    repository: &str,
    key: &str,
    work: impl FnOnce() -> T,
) -> Result<T, String> {
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
    let done = work();
    crate::warn_unrecorded(
        "release the gate cache lease the storage worker took",
        coordinator.release(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        ),
    );
    Ok(done)
}

/// Remove a candidate: renamed aside first (so a half-removed tree is never
/// mistaken for a valid one), then deleted without following links;
/// `Ok(false)` when `deadline` stopped the deletion (the renamed remainder
/// is swept by [`sweep_aside`] on the next pass).
pub fn remove(
    candidate: &Candidate,
    beat: &mut dyn FnMut(),
    deadline: Option<Instant>,
) -> std::io::Result<bool> {
    let path = candidate.path();
    let aside = candidate.root.join(format!(
        ".aethyme-storage-evicting-{}-{}",
        std::process::id(),
        super::now_ms()
    ));
    fs::rename(&path, &aside)?;
    delete_tree(&aside, beat, deadline)
}

/// Delete a tree bottom up, never following a link, refreshing `beat` at
/// least every second; `Ok(false)` when `deadline` passed first.
pub fn delete_tree(
    path: &Path,
    beat: &mut dyn FnMut(),
    deadline: Option<Instant>,
) -> std::io::Result<bool> {
    delete_tree_measured(path, beat, deadline).map(|(done, _)| done)
}

/// [`delete_tree`], also returning the allocated bytes of what it removed.
pub fn delete_tree_measured(
    path: &Path,
    beat: &mut dyn FnMut(),
    deadline: Option<Instant>,
) -> std::io::Result<(bool, u64)> {
    let mut removed_bytes = 0_u64;
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
                removed_bytes = removed_bytes.saturating_add(super::allocated(&metadata));
            } else {
                stack.push((next.clone(), true));
                for entry in fs::read_dir(&next)?.flatten() {
                    stack.push((entry.path(), false));
                }
            }
        } else {
            // A file or a link: the link itself goes, never its target.
            fs::remove_file(&next)?;
            removed_bytes = removed_bytes.saturating_add(super::allocated(&metadata));
        }
        if last_beat.elapsed() >= super::BEAT_EVERY {
            beat();
            last_beat = Instant::now();
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) && !stack.is_empty() {
            return Ok((false, removed_bytes));
        }
    }
    Ok((true, removed_bytes))
}

/// Leftovers of removals interrupted after the rename.
pub fn sweep_aside(root: &Path, beat: &mut dyn FnMut()) {
    for name in super::read_names(root) {
        if !name.starts_with(".aethyme-storage-evicting-") {
            continue;
        }
        crate::warn_unrecorded(
            "remove an interrupted storage eviction",
            delete_tree(&root.join(&name), beat, None).map(drop),
        );
    }
}
