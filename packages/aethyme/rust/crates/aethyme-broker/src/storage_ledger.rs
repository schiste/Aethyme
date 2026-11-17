//! One ledger of everything Aethyme stores on a host, with a hard budget per
//! class, a total per root and a free-space floor per volume.
//!
//! Aethyme writes in three kinds of root: the host state directory, the host
//! cache directory, and storage roots (the `Aethyme` directory that holds a
//! repository's worktree root or preparation cache, often on another volume).
//! Every top-level entry of a root is classified by name ([`classify`]);
//! anything that does not match one of Aethyme's own names is
//! `unclassified`, listed with its size and never deleted.
//!
//! Sizes are never measured inside a command. The detached worker
//! ([`check::run_worker_pass`]) re-measures a few entries per pass, round
//! robin, and persists the result in `<host state>/storage-ledger.json`; a
//! small summary beside it is all a command reads ([`check::command_start`]),
//! so `aethyme storage status` and the start-of-command check cost
//! milliseconds and never walk a tree.
//!
//! Classes that are checkout removal (worktrees and their recovery copies)
//! belong to another component and are only measured here. Deletion sites and
//! why each is safe are in [`evict`].

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub mod check;
pub mod cli;
pub mod evict;
#[cfg(test)]
mod tests;

/// The ledger file in the host state directory.
pub const LEDGER_FILE: &str = "storage-ledger.json";
/// The small summary every command reads.
pub const SUMMARY_FILE: &str = "storage-ledger-summary.json";
/// Optional owner overrides of the budgets.
pub const BUDGET_FILE: &str = "storage.toml";
const LEDGER_SCHEMA: u32 = 1;
const GIB: u64 = 1024 * 1024 * 1024;
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const HOUR: Duration = Duration::from_secs(60 * 60);

/// What a top-level entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    RecoveryArchives,
    GateCache,
    SymbolIndex,
    GraphStores,
    PreparationCache,
    QualificationRetained,
    ReleaseArtifacts,
    RolloutArtifacts,
    QuickTestPools,
    ToolCaches,
    Worktrees,
    Coordination,
    Unclassified,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RecoveryArchives => "recovery_archives",
            Self::GateCache => "gate_cache",
            Self::SymbolIndex => "symbol_index",
            Self::GraphStores => "graph_stores",
            Self::PreparationCache => "preparation_cache",
            Self::QualificationRetained => "qualification_retained",
            Self::ReleaseArtifacts => "release_artifacts",
            Self::RolloutArtifacts => "rollout_artifacts",
            Self::QuickTestPools => "quick_test_pools",
            Self::ToolCaches => "tool_caches",
            Self::Worktrees => "worktrees",
            Self::Coordination => "coordination",
            Self::Unclassified => "unclassified",
        }
    }
}

/// Which kind of root an entry lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    HostState,
    HostCache,
    Storage,
}

/// How a class is kept within its budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Never deleted here: owned by another component, or live state.
    ReportOnly(&'static str),
    /// Children are evicted least recently used first while the class is
    /// over budget, except those used within `idle`; children older than
    /// `expire` go whatever the budget.
    LruChildren {
        idle: Duration,
        expire: Option<Duration>,
    },
    /// The whole entry goes once nothing in it changed for `age`.
    ExpireEntry { age: Duration },
    /// Over budget, every entry but the most recently used one per gate
    /// kind goes; an entry with a live lease is never touched.
    GateCache,
    /// Oldest first, only archives whose content is proven landed in Git or
    /// that are older than the retention.
    RecoveryArchives,
}

/// The class of the top-level entry `name` in a root of `kind`.
pub fn classify(kind: RootKind, name: &str) -> Class {
    let starts = |prefix: &str| name.starts_with(prefix);
    if name.contains("-quick-test-") {
        return Class::QuickTestPools;
    }
    match kind {
        RootKind::HostState => match name {
            "recovery-archives" => Class::RecoveryArchives,
            "qualification-retained" => Class::QualificationRetained,
            "worktrees" => Class::Worktrees,
            _ if starts("preparation-cache") && !name.contains("activity") => {
                Class::PreparationCache
            }
            _ if starts("worktree-roots-") || starts("worktree-watch") => Class::Worktrees,
            _ if starts("rollout-v")
                || starts("rollout-prep-")
                || starts("rollout-before-files-")
                || starts("preupgrade-db-backup-") =>
            {
                Class::RolloutArtifacts
            }
            _ if starts("release-") => Class::ReleaseArtifacts,
            "collaboration"
            | "console-markers"
            | "environments"
            | "environments.db"
            | "gate-trust"
            | "gc-sweep-services"
            | "host-operations.db"
            | "host-resources.db"
            | "operation-locks"
            | "preparation-cache-activity"
            | "preparation-rotation-recovery"
            | "rollout-holds"
            | "run"
            | "watch-services"
            | "mcp"
            | LEDGER_FILE
            | SUMMARY_FILE
            | BUDGET_FILE
            | check::LOCK_FILE
            | check::ROOTS_FILE
            | check::GATE_BUDGETS_FILE => Class::Coordination,
            _ if name.starts_with("host-") && name.contains(".db") => Class::Coordination,
            _ => Class::Unclassified,
        },
        RootKind::HostCache => match name {
            "gates" => Class::GateCache,
            "symbol-index" => Class::SymbolIndex,
            "graph-stores" => Class::GraphStores,
            "cargo" | "git-probe" | "update-manifests" => Class::ToolCaches,
            "worktrees" => Class::Worktrees,
            _ => Class::Unclassified,
        },
        RootKind::Storage => match name {
            "worktrees" | "checkout-preserved" => Class::Worktrees,
            _ if starts("worktrees-recovery-") => Class::Worktrees,
            "qualification-retained" => Class::QualificationRetained,
            "build-cache"
            | "cache"
            | "task-caches"
            | "private-caches"
            | "private-pnpm-stores"
            | "private-stores"
            | "target" => Class::ToolCaches,
            _ if starts("uv-cache-") => Class::ToolCaches,
            _ if starts("preparation-cache") || starts("schema-preparation-cache") => {
                Class::PreparationCache
            }
            _ if starts("release-") => Class::ReleaseArtifacts,
            _ if starts("rollout-v") || starts("rollout-prep-") => Class::RolloutArtifacts,
            _ => Class::Unclassified,
        },
    }
}

/// How `class` is enforced.
pub fn policy(class: Class) -> Policy {
    match class {
        Class::RecoveryArchives => Policy::RecoveryArchives,
        Class::GateCache => Policy::GateCache,
        Class::SymbolIndex | Class::GraphStores => Policy::LruChildren {
            idle: HOUR,
            expire: None,
        },
        Class::QualificationRetained => Policy::LruChildren {
            idle: DAY,
            expire: Some(14 * DAY),
        },
        Class::ToolCaches => Policy::LruChildren {
            idle: 7 * DAY,
            expire: None,
        },
        Class::ReleaseArtifacts | Class::RolloutArtifacts => Policy::ExpireEntry { age: 14 * DAY },
        Class::QuickTestPools => Policy::ExpireEntry { age: DAY },
        Class::PreparationCache => {
            Policy::ReportOnly("rotated by the preparation cache, which knows what is in use")
        }
        Class::Worktrees => Policy::ReportOnly("checkouts are removed by checkout cleanup"),
        Class::Coordination => Policy::ReportOnly("live coordination state"),
        Class::Unclassified => Policy::ReportOnly("not a name Aethyme writes; never deleted"),
    }
}

/// The budgets, from defaults and `<host state>/storage.toml` `[budget]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Budgets {
    pub recovery_archives_bytes: u64,
    /// Per repository: each repository's gate caches are judged on their own.
    pub gate_cache_bytes_per_repository: u64,
    pub symbol_index_bytes: u64,
    pub graph_stores_bytes: u64,
    pub qualification_retained_bytes: u64,
    pub tool_caches_bytes: u64,
    pub preparation_cache_bytes: u64,
    pub host_state_total_bytes: u64,
    pub host_cache_total_bytes: u64,
    pub storage_total_bytes: u64,
    /// Free space each volume holding a root keeps.
    pub free_floor_bytes: u64,
    /// Archives older than this go whatever their content.
    pub recovery_archive_retention_days: u64,
    /// Each repository's own `gate_cache_bytes_budget`, as its broker last
    /// registered it; it wins over the host default.
    #[serde(skip)]
    pub gate_cache_by_repository: BTreeMap<String, u64>,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            recovery_archives_bytes: 4 * GIB,
            gate_cache_bytes_per_repository: 4 * GIB,
            symbol_index_bytes: 2 * GIB,
            graph_stores_bytes: 2 * GIB,
            qualification_retained_bytes: GIB,
            tool_caches_bytes: 8 * GIB,
            preparation_cache_bytes: 4 * GIB,
            host_state_total_bytes: 16 * GIB,
            host_cache_total_bytes: 12 * GIB,
            storage_total_bytes: 48 * GIB,
            free_floor_bytes: 10 * GIB,
            recovery_archive_retention_days: 30,
            gate_cache_by_repository: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct BudgetFile {
    #[serde(default)]
    budget: Budgets,
}

impl Budgets {
    /// Defaults overridden by `<host state>/storage.toml`; an unreadable or
    /// invalid file keeps the defaults and says why.
    pub fn load(host_state: &Path) -> (Self, Option<String>) {
        let (mut budgets, error) = match fs::read_to_string(host_state.join(BUDGET_FILE)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => (Self::default(), None),
            Err(error) => (Self::default(), Some(format!("{BUDGET_FILE}: {error}"))),
            Ok(text) => match toml::from_str::<BudgetFile>(&text) {
                Ok(file) => (file.budget, None),
                Err(error) => (Self::default(), Some(format!("{BUDGET_FILE}: {error}"))),
            },
        };
        budgets.gate_cache_by_repository = fs::read(host_state.join(check::GATE_BUDGETS_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        (budgets, error)
    }

    /// A repository's gate cache budget: its configured one, else the
    /// host default.
    pub fn gate_budget(&self, repository: &str) -> u64 {
        self.gate_cache_by_repository
            .get(repository)
            .copied()
            .unwrap_or(self.gate_cache_bytes_per_repository)
    }

    /// The class budget, if the class has one. The gate cache budget is per
    /// repository and applied by [`evict`].
    pub fn class_bytes(&self, class: Class) -> Option<u64> {
        match class {
            Class::RecoveryArchives => Some(self.recovery_archives_bytes),
            Class::SymbolIndex => Some(self.symbol_index_bytes),
            Class::GraphStores => Some(self.graph_stores_bytes),
            Class::QualificationRetained => Some(self.qualification_retained_bytes),
            Class::ToolCaches => Some(self.tool_caches_bytes),
            Class::PreparationCache => Some(self.preparation_cache_bytes),
            _ => None,
        }
    }

    /// The total for one kind of root (worktrees excluded).
    pub fn root_total(&self, kind: RootKind) -> u64 {
        match kind {
            RootKind::HostState => self.host_state_total_bytes,
            RootKind::HostCache => self.host_cache_total_bytes,
            RootKind::Storage => self.storage_total_bytes,
        }
    }
}

/// One root Aethyme writes in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Root {
    pub path: PathBuf,
    pub kind: RootKind,
}

/// One measured child of an entry (an archive, a gate cache key, an index).
/// Two-level classes name it `<group>/<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Child {
    pub name: String,
    pub bytes: u64,
    /// The most recent change inside it.
    pub newest_ms: u64,
}

/// One top-level entry of a root, as last measured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub root: PathBuf,
    pub root_kind: RootKind,
    pub name: String,
    pub class: Class,
    /// The volume (device) the root is on.
    pub device: u64,
    pub bytes: u64,
    pub entries: u64,
    pub oldest_ms: u64,
    pub newest_ms: u64,
    /// 0 until first measured.
    pub measured_at_ms: u64,
    /// Kept for classes that are evicted child by child.
    #[serde(default)]
    pub children: Vec<Child>,
}

impl Item {
    pub fn path(&self) -> PathBuf {
        self.root.join(&self.name)
    }
}

/// Counters per class, kept across passes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassTelemetry {
    pub evictions: u64,
    pub bytes_reclaimed: u64,
    /// When the class went over budget, while it still is.
    pub over_budget_since_ms: Option<u64>,
    /// Time spent over budget, excluding the current stretch.
    pub over_budget_ms_total: u64,
}

/// Free space of one volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    pub device: u64,
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub measured_at_ms: u64,
    /// Times a pass found it below the floor.
    #[serde(default)]
    pub floor_breaches: u64,
}

/// What the last worker pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassReport {
    pub started_at_ms: u64,
    pub phases_ms: BTreeMap<String, u64>,
    pub measured: Vec<String>,
    pub evicted: Vec<String>,
    pub bytes_reclaimed: u64,
    pub skipped: Vec<String>,
    /// Recovery archives kept over budget because their work is not landed.
    pub unlanded_archives: Vec<String>,
    pub pressure: bool,
}

/// The persisted ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    pub schema: u32,
    pub updated_at_ms: u64,
    pub roots: Vec<Root>,
    pub items: Vec<Item>,
    pub volumes: Vec<Volume>,
    pub telemetry: BTreeMap<Class, ClassTelemetry>,
    /// Round-robin position of the next entry to measure.
    pub cursor: usize,
    pub last_pass: Option<PassReport>,
}

impl Ledger {
    pub fn load(host_state: &Path) -> Self {
        fs::read(host_state.join(LEDGER_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .filter(|ledger| ledger.schema == LEDGER_SCHEMA)
            .unwrap_or_else(|| Self {
                schema: LEDGER_SCHEMA,
                ..Self::default()
            })
    }

    pub fn save(&self, host_state: &Path) -> io::Result<()> {
        write_atomically(
            &host_state.join(LEDGER_FILE),
            &serde_json::to_vec(self).map_err(io::Error::other)?,
        )
    }

    /// Bytes per class.
    pub fn class_bytes(&self) -> BTreeMap<Class, u64> {
        let mut totals = BTreeMap::new();
        for item in &self.items {
            *totals.entry(item.class).or_insert(0) += item.bytes;
        }
        totals
    }

    /// Bytes per root (worktrees excluded: another component removes them).
    pub fn root_bytes(&self) -> BTreeMap<RootKind, u64> {
        let mut totals = BTreeMap::new();
        for item in self
            .items
            .iter()
            .filter(|item| item.class != Class::Worktrees)
        {
            *totals.entry(item.root_kind).or_insert(0) += item.bytes;
        }
        totals
    }

    /// What is over budget now: `(class or root name, bytes, budget)`.
    pub fn over_budget(&self, budgets: &Budgets) -> Vec<(String, u64, u64)> {
        let mut over = Vec::new();
        for (class, bytes) in self.class_bytes() {
            if let Some(budget) = budgets.class_bytes(class)
                && bytes > budget
            {
                over.push((class.as_str().to_string(), bytes, budget));
            }
        }
        for (repository, bytes) in evict::gate_cache_by_repository(self) {
            let budget = budgets.gate_budget(&repository);
            if bytes > budget {
                over.push((format!("gate_cache:{repository}"), bytes, budget));
            }
        }
        for (kind, bytes) in self.root_bytes() {
            let budget = budgets.root_total(kind);
            if bytes > budget {
                over.push((format!("total:{}", root_kind_name(kind)), bytes, budget));
            }
        }
        over
    }
}

pub fn root_kind_name(kind: RootKind) -> &'static str {
    match kind {
        RootKind::HostState => "host_state",
        RootKind::HostCache => "host_cache",
        RootKind::Storage => "storage",
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

fn mtime_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    io::Write::write_all(&mut temporary, bytes)?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

thread_local! {
    /// Tree walks performed by this thread, so tests can prove a path never
    /// walks.
    pub(crate) static WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What a walk found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub bytes: u64,
    pub entries: u64,
    pub oldest_ms: u64,
    pub newest_ms: u64,
}

/// Allocated bytes, entries and modification range of `path`, without
/// following symbolic links.
pub fn measure(path: &Path) -> Usage {
    WALKS.with(|walks| walks.set(walks.get() + 1));
    let mut usage = Usage::default();
    let mut pending = vec![path.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(metadata) = fs::symlink_metadata(&next) else {
            continue;
        };
        usage.entries += 1;
        usage.bytes += allocated(&metadata);
        let modified = mtime_ms(&metadata);
        usage.newest_ms = usage.newest_ms.max(modified);
        usage.oldest_ms = if usage.oldest_ms == 0 {
            modified
        } else {
            usage.oldest_ms.min(modified)
        };
        if metadata.is_dir()
            && let Ok(entries) = fs::read_dir(&next)
        {
            pending.extend(entries.flatten().map(|entry| entry.path()));
        }
    }
    usage
}

#[cfg(unix)]
fn allocated(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}

#[cfg(unix)]
pub(crate) fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|metadata| metadata.dev())
}

#[cfg(not(unix))]
pub(crate) fn device_of(_path: &Path) -> Option<u64> {
    Some(0)
}

/// The children worth keeping for an entry of `class`: `<group>/<name>` for
/// two-level classes, `<name>` for the others; empty when the class is not
/// evicted child by child.
pub fn measure_children(class: Class, path: &Path) -> Vec<Child> {
    let two_level = matches!(class, Class::RecoveryArchives | Class::GateCache);
    let one_level = matches!(policy(class), Policy::LruChildren { .. });
    if !two_level && !one_level {
        return Vec::new();
    }
    let mut children = Vec::new();
    for group in read_names(path) {
        let group_path = path.join(&group);
        if two_level {
            if !group_path.is_dir() {
                continue;
            }
            for name in read_names(&group_path) {
                let usage = measure(&group_path.join(&name));
                children.push(Child {
                    name: format!("{group}/{name}"),
                    bytes: usage.bytes,
                    newest_ms: usage.newest_ms,
                });
            }
        } else {
            let usage = measure(&group_path);
            children.push(Child {
                name: group,
                bytes: usage.bytes,
                newest_ms: usage.newest_ms,
            });
        }
    }
    children
}

/// Names in a directory (UTF-8 only; others are left alone and unmeasured).
pub(crate) fn read_names(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Add top-level entries that appeared under the roots and drop those that
/// disappeared; sizes of new entries are unknown (0) until measured.
pub fn discover(ledger: &mut Ledger) {
    let mut seen = Vec::new();
    for root in &ledger.roots {
        let Some(device) = device_of(&root.path) else {
            continue;
        };
        for name in read_names(&root.path) {
            if name.starts_with(".aethyme-storage-") {
                continue;
            }
            seen.push((root.path.clone(), name.clone()));
            let known = ledger
                .items
                .iter()
                .any(|item| item.root == root.path && item.name == name);
            if !known {
                ledger.items.push(Item {
                    root: root.path.clone(),
                    root_kind: root.kind,
                    class: classify(root.kind, &name),
                    name,
                    device,
                    bytes: 0,
                    entries: 0,
                    oldest_ms: 0,
                    newest_ms: 0,
                    measured_at_ms: 0,
                    children: Vec::new(),
                });
            }
        }
    }
    ledger.items.retain(|item| {
        seen.iter()
            .any(|(root, name)| *root == item.root && *name == item.name)
    });
}

/// Measure entries round robin until `budget` elapses, never-measured
/// entries first; the names measured.
pub fn measure_some(ledger: &mut Ledger, budget: Duration) -> Vec<String> {
    let started = std::time::Instant::now();
    let mut measured = Vec::new();
    if ledger.items.is_empty() {
        return measured;
    }
    let mut order: Vec<usize> = (0..ledger.items.len())
        .filter(|index| ledger.items[*index].measured_at_ms == 0)
        .collect();
    let count = ledger.items.len();
    order.extend((0..count).map(|offset| (ledger.cursor + offset) % count));
    let mut done = std::collections::BTreeSet::new();
    for index in order {
        if started.elapsed() >= budget {
            break;
        }
        if !done.insert(index) {
            continue;
        }
        let item = &mut ledger.items[index];
        let path = item.path();
        let usage = measure(&path);
        item.bytes = usage.bytes;
        item.entries = usage.entries;
        item.oldest_ms = usage.oldest_ms;
        item.newest_ms = usage.newest_ms;
        item.children = measure_children(item.class, &path);
        item.measured_at_ms = now_ms();
        measured.push(format!("{}:{}", root_kind_name(item.root_kind), item.name));
        ledger.cursor = (index + 1) % count;
    }
    measured
}

/// Record over-budget time for each class.
pub fn update_over_budget_clock(ledger: &mut Ledger, budgets: &Budgets, now: u64) {
    let totals = ledger.class_bytes();
    for class in totals.keys().copied().collect::<Vec<_>>() {
        let over = budgets
            .class_bytes(class)
            .is_some_and(|budget| totals.get(&class).copied().unwrap_or(0) > budget);
        let telemetry = ledger.telemetry.entry(class).or_default();
        match (over, telemetry.over_budget_since_ms) {
            (true, None) => telemetry.over_budget_since_ms = Some(now),
            (false, Some(since)) => {
                telemetry.over_budget_ms_total += now.saturating_sub(since);
                telemetry.over_budget_since_ms = None;
            }
            _ => {}
        }
    }
}

impl crate::Broker {
    /// `broker status` advice from the storage ledger; also registers this
    /// repository's storage roots (its worktree root's and preparation
    /// cache's Aethyme directory) so the worker inventories them. Cheap: one
    /// small file read and a statvfs per root.
    pub(crate) fn storage_ledger_advice(&self) -> Vec<crate::StatusAdvice> {
        let Some(host_state) = self.storage_context().host_state_dir.clone() else {
            return Vec::new();
        };
        if !host_state.is_dir() || !check::enabled(&host_state) {
            return Vec::new();
        }
        let mut storage_roots = Vec::new();
        if let Ok(plan) = self.worktree_root_plan() {
            if let Ok(policy) = crate::retention::load_retention_policy(self.main_root()) {
                check::register_gate_budget(
                    &host_state,
                    &plan.repository_key,
                    policy.gate_cache_bytes_budget,
                );
            }
            // The configured root's parent, or the keyed container's
            // grandparent: whichever is an Aethyme directory.
            storage_roots.extend(
                plan.preferred_root
                    .as_deref()
                    .and_then(check::storage_root_for),
            );
            storage_roots.extend(
                plan.root_container
                    .as_deref()
                    .and_then(Path::parent)
                    .and_then(check::storage_root_for),
            );
        }
        if let Ok(Some(preparation)) = self.storage_context().preparation_cache_root_binding()
            && let Some(root) = check::storage_root_for(&preparation.path)
        {
            storage_roots.push(root);
        }
        check::register_storage_roots(&host_state, &storage_roots);
        cli::status_advice(&host_state)
    }
}
