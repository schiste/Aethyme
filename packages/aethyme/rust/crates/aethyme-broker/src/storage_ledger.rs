//! One ledger of everything Aethyme stores on a host, with a hard budget per
//! class, a total per root and a free-space floor per volume.
//!
//! Roots are exact paths, never guessed from names: the host state directory,
//! the host cache directory, each repository's configured worktree root (as
//! its broker registers it), and storage directories an owner adopted with
//! `aethyme storage adopt`, which writes the [`STORAGE_ROOT_MARKER`] a root
//! must carry. A repository's `.aethyme/` is never a root.
//!
//! Inside a root, a top-level entry gets a class only by an exact name, or an
//! exact generated pattern with a validated suffix, and only in the one kind
//! of root where Aethyme creates it ([`classify`]). Anything else is
//! `unclassified`: listed, never deleted.
//!
//! Sizes are never measured inside a command. The detached worker
//! ([`check::run_worker_pass`]) measures evictable entries with a deadline
//! checked inside the walk and a persisted, resumable cursor; it never
//! crosses a mount point and never walks a class it does not remove
//! (worktrees are counted from their directory entries only). A small
//! summary is all a command reads ([`check::command_start`]).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
/// Producers' notes of entries they wrote (`<host state>/storage-ledger-dirty`).
pub const DIRTY_DIRECTORY: &str = "storage-ledger-dirty";
/// The file `aethyme storage adopt` writes into a storage root.
pub const STORAGE_ROOT_MARKER: &str = ".aethyme-storage-root";
const LEDGER_SCHEMA: u32 = 2;
const GIB: u64 = 1024 * 1024 * 1024;
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const HOUR: Duration = Duration::from_secs(60 * 60);
/// Directory entries counted for a class that is not walked.
const COUNT_LIMIT: u64 = 100_000;
/// How often a walk refreshes the worker's heartbeat.
pub(crate) const BEAT_EVERY: Duration = Duration::from_secs(1);

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
    /// A repository's configured worktree root.
    WorktreeRoot,
    /// A directory an owner adopted (it carries [`STORAGE_ROOT_MARKER`]).
    Storage,
}

/// How a class is kept within its budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Never deleted here, and never walked: owned by another component, or
    /// live state, or not Aethyme's.
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
    /// Over budget, oldest first, only archives proven landed and past their
    /// recorded minimum retention; never by age alone.
    RecoveryArchives,
}

fn all(text: &str, allowed: impl Fn(char) -> bool) -> bool {
    !text.is_empty() && text.chars().all(allowed)
}

/// `<major>.<minor>.<patch>` followed by `-` tokens of `[A-Za-z0-9.]`.
fn versioned_suffix(rest: &str) -> bool {
    let (version, tail) = match rest.split_once('-') {
        Some((version, tail)) => (version, Some(tail)),
        None => (rest, None),
    };
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|part| all(part, |c| c.is_ascii_digit()))
        && tail.is_none_or(|tail| {
            tail.split('-')
                .all(|token| all(token, |c| c.is_ascii_alphanumeric() || c == '.'))
        })
}

/// `aethyme-broker-quick-test-<pid>-<time>-<16 hex>`: the worktree container
/// of a broker quick-test fixture repository.
pub fn is_quick_test_pool(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("aethyme-broker-quick-test-") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').collect();
    parts.len() == 3
        && all(parts[0], |c| c.is_ascii_digit())
        && all(parts[1], |c| c.is_ascii_digit())
        && parts[2].len() == 16
        && all(parts[2], |c| matches!(c, '0'..='9' | 'a'..='f'))
}

/// The class of the top-level entry `name` in a root of `kind`.
pub fn classify(kind: RootKind, name: &str) -> Class {
    match kind {
        RootKind::HostState => {
            if let Some(rest) = [
                "rollout-v",
                "rollout-prep-v",
                "rollout-before-files-v",
                "preupgrade-db-backup-v",
            ]
            .iter()
            .find_map(|prefix| name.strip_prefix(prefix))
                && versioned_suffix(rest)
            {
                return Class::RolloutArtifacts;
            }
            if let Some(rest) = ["release-assets-v", "release-recovery-v"]
                .iter()
                .find_map(|prefix| name.strip_prefix(prefix))
                && versioned_suffix(rest)
            {
                return Class::ReleaseArtifacts;
            }
            if let Some(hash) = name.strip_prefix("worktree-roots-")
                && hash.len() == 64
                && all(hash, |c| c.is_ascii_hexdigit())
            {
                return Class::Worktrees;
            }
            match name {
                "recovery-archives" => Class::RecoveryArchives,
                "qualification-retained" => Class::QualificationRetained,
                "preparation-cache" => Class::PreparationCache,
                "worktrees" | "worktree-watch-v1" | "worktree-watch-v2" => Class::Worktrees,
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
                | DIRTY_DIRECTORY
                | check::LOCK_FILE
                | check::ROOTS_FILE
                | check::GATE_BUDGETS_FILE => Class::Coordination,
                _ => Class::Unclassified,
            }
        }
        RootKind::HostCache => match name {
            "gates" => Class::GateCache,
            "symbol-index" => Class::SymbolIndex,
            "graph-stores" => Class::GraphStores,
            "cargo" | "git-probe" | "update-manifests" => Class::ToolCaches,
            "worktrees" => Class::Worktrees,
            _ => Class::Unclassified,
        },
        // Repository worktree containers: removed by checkout cleanup only.
        RootKind::WorktreeRoot if is_quick_test_pool(name) => Class::QuickTestPools,
        RootKind::WorktreeRoot => Class::Worktrees,
        RootKind::Storage => {
            if let Some(rest) = name.strip_prefix("release-")
                && let Some((number, tail)) = rest.split_once('-')
                && number.len() == 4
                && all(number, |c| c.is_ascii_digit())
                && all(tail, |c| {
                    c.is_ascii_alphanumeric() || matches!(c, '.' | '-')
                })
            {
                return Class::ReleaseArtifacts;
            }
            match name {
                "build-cache"
                | "private-caches"
                | "private-pnpm-stores"
                | "private-stores"
                | "task-caches" => Class::ToolCaches,
                "qualification-retained" => Class::QualificationRetained,
                "preparation-cache" | "preparation-cache-isolated" => Class::PreparationCache,
                "worktrees" | "checkout-preserved" => Class::Worktrees,
                STORAGE_ROOT_MARKER => Class::Coordination,
                _ => Class::Unclassified,
            }
        }
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
    /// Below this free space on a volume, eviction runs unpaced and advice
    /// warns.
    pub free_floor_bytes: u64,
    /// Below this, commands that write to that volume refuse once a bounded
    /// reclaim did not free enough.
    pub hard_minimum_bytes: u64,
    /// Recovery archives, landed or not, expire this many days after their
    /// manifest's creation (never sooner than their own minimum retention,
    /// nor than a week after the ledger first saw them); 0 turns it off.
    pub unlanded_archive_days: u64,
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
            hard_minimum_bytes: 2 * GIB,
            unlanded_archive_days: 30,
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

    /// The class budget, if the class has one.
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
            RootKind::WorktreeRoot | RootKind::Storage => self.storage_total_bytes,
        }
    }
}

/// One root Aethyme writes in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Root {
    pub path: PathBuf,
    pub kind: RootKind,
    /// Worktree roots: the main checkouts of the repositories whose
    /// configured worktree root it is.
    #[serde(default)]
    pub repositories: Vec<PathBuf>,
}

impl Root {
    pub fn new(path: &Path, kind: RootKind) -> Self {
        Self {
            path: path.to_path_buf(),
            kind,
            repositories: Vec::new(),
        }
    }

    /// Whether a command running in `cwd` writes to this root: always for
    /// the host state and cache; for a worktree root only from inside it or
    /// from one of its repositories.
    pub fn written_from(&self, cwd: Option<&Path>) -> bool {
        match self.kind {
            RootKind::WorktreeRoot => cwd.is_some_and(|cwd| {
                cwd.starts_with(&self.path)
                    || self
                        .repositories
                        .iter()
                        .any(|repository| cwd.starts_with(repository))
            }),
            _ => true,
        }
    }
}

/// One measured child of an entry (an archive, a gate cache key, an index).
/// Two-level classes name it `<group>/<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Child {
    pub name: String,
    pub bytes: u64,
    /// The most recent change inside it; 0 when unknown.
    pub newest_ms: u64,
    /// Its own status-change time; 0 when unknown.
    #[serde(default)]
    pub ctime_ms: u64,
    /// Recovery archives: the manifest's creation time and minimum
    /// retention.
    #[serde(default)]
    pub created_ms: Option<u64>,
    #[serde(default)]
    pub min_retention_ms: Option<u64>,
    /// Recovery archives: the session the manifest names.
    #[serde(default)]
    pub session_id: Option<i64>,
    /// Recovery archives: an owner's keep marker exempts it from expiry.
    #[serde(default)]
    pub kept: bool,
}

/// A walk in progress, persisted so the next pass resumes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Walk {
    /// Children not started yet.
    pub children_left: Vec<String>,
    /// The child being walked, its pending paths and what was found so far.
    pub current: Option<String>,
    pub pending: Vec<PathBuf>,
    pub usage: Usage,
    pub done: Vec<Child>,
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
    /// Meaningful only when `measured`.
    pub bytes: u64,
    pub measured: bool,
    pub entries: u64,
    pub oldest_ms: u64,
    pub newest_ms: u64,
    /// 0 until first measured.
    pub measured_at_ms: u64,
    /// Kept for classes that are evicted child by child.
    #[serde(default)]
    pub children: Vec<Child>,
    /// A walk the deadline interrupted.
    #[serde(default)]
    pub walk: Option<Walk>,
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

/// Recovery archive expiry counters: removed by age (cumulative), and, as
/// of the last pass, kept by a marker and expiring within the notice week.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveTelemetry {
    pub expired: u64,
    pub expired_bytes: u64,
    pub kept: u64,
    pub kept_bytes: u64,
    pub noticed: u64,
    pub noticed_bytes: u64,
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
    /// Entries whose walk the deadline interrupted (resumed next pass).
    pub resumable: Vec<String>,
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
    /// Recovery archives announced as expiring (`<pool>/<id>`): when first
    /// published, in how many passes, and when last.
    #[serde(default)]
    pub archive_notices: BTreeMap<String, Notice>,
    /// Set for one pass when the clock went back or passes stopped for over
    /// a week: nothing is removed by age in that pass.
    #[serde(skip)]
    pub age_frozen: bool,
    #[serde(default)]
    pub archive_telemetry: ArchiveTelemetry,
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

    /// Measured bytes per class.
    pub fn class_bytes(&self) -> BTreeMap<Class, u64> {
        let mut totals = BTreeMap::new();
        for item in self.items.iter().filter(|item| item.measured) {
            *totals.entry(item.class).or_insert(0) += item.bytes;
        }
        totals
    }

    /// Measured bytes per root (worktrees are never measured here).
    pub fn root_bytes(&self) -> BTreeMap<RootKind, u64> {
        let mut totals = BTreeMap::new();
        for item in self.items.iter().filter(|item| item.measured) {
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
        RootKind::WorktreeRoot => "worktree_root",
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

pub(crate) fn mtime_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(unix)]
pub(crate) fn ctime_ms(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    u64::try_from(metadata.ctime())
        .unwrap_or(0)
        .saturating_mul(1000)
}

#[cfg(not(unix))]
pub(crate) fn ctime_ms(_metadata: &fs::Metadata) -> u64 {
    0
}

pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    io::Write::write_all(&mut temporary, bytes)?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

thread_local! {
    /// Entries visited by walks on this thread, so tests can prove a path
    /// never walks.
    pub(crate) static WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What a walk found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub bytes: u64,
    pub entries: u64,
    pub oldest_ms: u64,
    pub newest_ms: u64,
}

impl Usage {
    fn add(&mut self, metadata: &fs::Metadata) {
        self.entries += 1;
        self.bytes += allocated(metadata);
        let modified = mtime_ms(metadata);
        self.newest_ms = self.newest_ms.max(modified);
        self.oldest_ms = if self.oldest_ms == 0 {
            modified
        } else {
            self.oldest_ms.min(modified)
        };
    }
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
    fs::symlink_metadata(path)
        .ok()
        .map(|metadata| metadata.dev())
}

#[cfg(not(unix))]
pub(crate) fn device_of(_path: &Path) -> Option<u64> {
    Some(0)
}

#[cfg(unix)]
fn device(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.dev()
}

#[cfg(not(unix))]
fn device(_metadata: &fs::Metadata) -> u64 {
    0
}

/// Walk `pending` until it is empty (`true`) or `deadline` passes
/// (`false`, with `pending` and `usage` holding where it stopped). Never
/// follows a symbolic link and never enters another device than `root_device`.
/// `beat` is called at least every `beat_every` ([`BEAT_EVERY`] in the
/// worker).
pub fn walk_until(
    pending: &mut Vec<PathBuf>,
    usage: &mut Usage,
    root_device: u64,
    deadline: Instant,
    beat_every: Duration,
    beat: &mut dyn FnMut(),
) -> bool {
    let mut last_beat = Instant::now();
    while let Some(next) = pending.pop() {
        WALKS.with(|walks| walks.set(walks.get() + 1));
        if let Ok(metadata) = fs::symlink_metadata(&next)
            && device(&metadata) == root_device
        {
            usage.add(&metadata);
            if metadata.is_dir()
                && let Ok(entries) = fs::read_dir(&next)
            {
                pending.extend(entries.flatten().map(|entry| entry.path()));
            }
        }
        if last_beat.elapsed() >= beat_every {
            beat();
            last_beat = Instant::now();
        }
        if Instant::now() >= deadline && !pending.is_empty() {
            return false;
        }
    }
    true
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

/// The children of an evictable entry: `<group>/<name>` for two-level
/// classes, `<name>` otherwise.
fn child_names(class: Class, path: &Path) -> Vec<String> {
    let two_level = matches!(class, Class::RecoveryArchives | Class::GateCache);
    let mut names = Vec::new();
    for group in read_names(path) {
        let group_path = path.join(&group);
        if two_level {
            if fs::symlink_metadata(&group_path).is_ok_and(|metadata| metadata.is_dir()) {
                names.extend(
                    read_names(&group_path)
                        .into_iter()
                        // Keep markers sit beside archives; they are not one.
                        .filter(|name| !name.ends_with(KEEP_SUFFIX))
                        .map(|name| format!("{group}/{name}")),
                );
            }
        } else {
            names.push(group);
        }
    }
    names
}

/// What a recovery archive's manifest says about its age.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArchiveMeta {
    pub created_ms: Option<u64>,
    pub min_retention_ms: Option<u64>,
    pub session_id: Option<i64>,
}

/// Read an archive manifest's creation time, minimum retention and session.
/// Age is only ever taken from here, never from file times.
pub(crate) fn archive_meta(path: &Path) -> ArchiveMeta {
    let manifest = path.join("manifest.json");
    let small = fs::metadata(&manifest).is_ok_and(|metadata| metadata.len() <= 32 * 1024 * 1024);
    let value: Option<serde_json::Value> = small
        .then(|| fs::read(&manifest).ok())
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let field = |name: &str| value.as_ref().and_then(|value| value.get(name)).cloned();
    // An implausible creation time is unknown: never expired by age.
    let now = now_ms();
    ArchiveMeta {
        created_ms: field("created_at_ms")
            .and_then(|value| value.as_u64())
            .filter(|created| {
                *created >= EARLIEST_MANIFEST_MS && *created <= now.saturating_add(MANIFEST_SKEW_MS)
            }),
        min_retention_ms: field("minimum_archive_retention_ms").and_then(|value| value.as_u64()),
        session_id: field("session_id").and_then(|value| value.as_i64()),
    }
}

/// Suffix of the keep marker written beside an archive directory (outside
/// it, so the archive's own verified file set never changes).
pub const KEEP_SUFFIX: &str = ".aethyme-keep";

/// The keep marker of the archive at `path`: `<archive>.aethyme-keep` beside it.
pub fn keep_marker(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(std::ffi::OsString::from)
        .unwrap_or_default();
    name.push(KEEP_SUFFIX);
    path.with_file_name(name)
}

/// How long an archive's expiry is announced before it can be removed.
pub const ARCHIVE_NOTICE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The notice must be published in two passes at least this far apart.
pub const NOTICE_REPEAT: Duration = Duration::from_secs(24 * 60 * 60);
/// At most this many archives, or bytes, expire by age in one pass.
pub const AGE_EXPIRY_PER_PASS: usize = 50;
pub const AGE_EXPIRY_BYTES_PER_PASS: u64 = GIB;

/// An expiry announcement, as published to the summary `broker status`
/// reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub noticed_at_ms: u64,
    pub passes: u32,
    pub last_published_ms: u64,
}

impl Notice {
    pub fn new(now: u64) -> Self {
        Self {
            noticed_at_ms: now,
            passes: 1,
            last_published_ms: now,
        }
    }

    /// Record one more publication (counted once per [`NOTICE_REPEAT`]).
    pub fn publish(&mut self, now: u64) {
        let repeat = u64::try_from(NOTICE_REPEAT.as_millis()).unwrap_or(u64::MAX);
        if now.saturating_sub(self.last_published_ms) >= repeat {
            self.passes = self.passes.saturating_add(1);
            self.last_published_ms = now;
        }
    }

    /// Whether the notice ran its course: a week since first published, in
    /// at least two passes a day or more apart.
    pub fn served(&self, now: u64) -> bool {
        let week = u64::try_from(ARCHIVE_NOTICE.as_millis()).unwrap_or(u64::MAX);
        now.saturating_sub(self.noticed_at_ms) >= week && self.passes >= 2
    }
}

/// When an archive's age is up: its manifest creation time plus the larger
/// of `unlanded_archive_days` and its own minimum retention. `None` when
/// expiry is off, the archive is kept, or its manifest has no valid creation
/// time (never removed by age). Removal also needs a served [`Notice`].
pub fn archive_expiry(child: &Child, budgets: &Budgets) -> Option<u64> {
    if budgets.unlanded_archive_days == 0 || child.kept {
        return None;
    }
    let created = child.created_ms?;
    let age = budgets
        .unlanded_archive_days
        .saturating_mul(24 * 60 * 60 * 1000)
        .max(
            child
                .min_retention_ms
                .unwrap_or(crate::retention::MIN_RECOVERY_ARCHIVE_RETENTION_MS),
        );
    Some(created.saturating_add(age))
}

/// Whether an archive may be removed by age now.
pub fn archive_deletable(
    child: &Child,
    budgets: &Budgets,
    notice: Option<&Notice>,
    now: u64,
) -> bool {
    archive_expiry(child, budgets).is_some_and(|expires| now >= expires)
        && notice.is_some_and(|notice| notice.served(now))
}

/// The earliest plausible manifest time (2017-07-14), and how far ahead of
/// this clock a manifest time may be.
const EARLIEST_MANIFEST_MS: u64 = 1_500_000_000_000;
const MANIFEST_SKEW_MS: u64 = 24 * 60 * 60 * 1000;

/// Add top-level entries that appeared under the roots and drop those that
/// disappeared.
pub fn discover(ledger: &mut Ledger) {
    let mut seen = Vec::new();
    for root in &ledger.roots {
        let Some(device) = device_of(&root.path) else {
            continue;
        };
        for name in read_names(&root.path) {
            if name.starts_with(".aethyme-storage-evicting-") {
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
                    measured: false,
                    entries: 0,
                    oldest_ms: 0,
                    newest_ms: 0,
                    measured_at_ms: 0,
                    children: Vec::new(),
                    walk: None,
                });
            }
        }
    }
    ledger.items.retain(|item| {
        seen.iter()
            .any(|(root, name)| *root == item.root && *name == item.name)
    });
}

/// Measure (or count) one item until `deadline`; `true` when it finished.
pub fn measure_item(item: &mut Item, deadline: Instant, beat: &mut dyn FnMut()) -> bool {
    let path = item.path();
    // A symbolic link entry is never walked, removed or counted into.
    let Ok(top) = fs::symlink_metadata(&path) else {
        return true;
    };
    if top.file_type().is_symlink() {
        item.measured = false;
        item.measured_at_ms = now_ms();
        return true;
    }
    if let Policy::ReportOnly(_) = policy(item.class) {
        // Counted from its directory entries only, never walked.
        item.entries = fs::read_dir(&path)
            .map(|entries| {
                entries
                    .take(usize::try_from(COUNT_LIMIT).unwrap_or(usize::MAX))
                    .count() as u64
            })
            .unwrap_or(1);
        item.measured = false;
        item.measured_at_ms = now_ms();
        return true;
    }
    let root_device = device(&top);
    let child_level = !matches!(policy(item.class), Policy::ExpireEntry { .. });
    let mut walk = item.walk.take().unwrap_or_else(|| {
        if child_level {
            Walk {
                children_left: child_names(item.class, &path).into_iter().rev().collect(),
                ..Walk::default()
            }
        } else {
            Walk {
                current: Some(String::new()),
                pending: vec![path.clone()],
                ..Walk::default()
            }
        }
    });
    loop {
        if walk.current.is_none() {
            let Some(next) = walk.children_left.pop() else {
                break;
            };
            walk.pending = vec![path.join(&next)];
            walk.usage = Usage::default();
            walk.current = Some(next);
        }
        if !walk_until(
            &mut walk.pending,
            &mut walk.usage,
            root_device,
            deadline,
            BEAT_EVERY,
            beat,
        ) {
            item.walk = Some(walk);
            return false;
        }
        let name = walk.current.take().unwrap_or_default();
        if child_level {
            let child_path = path.join(&name);
            let ctime = fs::symlink_metadata(&child_path).map_or(0, |metadata| ctime_ms(&metadata));
            let meta = if item.class == Class::RecoveryArchives {
                archive_meta(&child_path)
            } else {
                ArchiveMeta::default()
            };
            let kept = item.class == Class::RecoveryArchives && keep_marker(&child_path).exists();
            walk.done.push(Child {
                name,
                bytes: walk.usage.bytes,
                newest_ms: walk.usage.newest_ms,
                ctime_ms: ctime,
                created_ms: meta.created_ms,
                min_retention_ms: meta.min_retention_ms,
                session_id: meta.session_id,
                kept,
            });
        } else {
            item.bytes = walk.usage.bytes;
            item.entries = walk.usage.entries;
            item.oldest_ms = walk.usage.oldest_ms;
            item.newest_ms = walk.usage.newest_ms;
        }
        if Instant::now() >= deadline && !walk.children_left.is_empty() {
            item.walk = Some(walk);
            return false;
        }
    }
    if child_level {
        item.bytes = walk.done.iter().map(|child| child.bytes).sum();
        item.entries = walk.done.len() as u64;
        item.newest_ms = walk
            .done
            .iter()
            .map(|child| child.newest_ms)
            .max()
            .unwrap_or(0);
        item.oldest_ms = walk
            .done
            .iter()
            .map(|child| child.newest_ms)
            .filter(|ms| *ms > 0)
            .min()
            .unwrap_or(0);
        item.children = walk.done;
    }
    item.measured = true;
    item.measured_at_ms = now_ms();
    true
}

/// Measure entries round robin until `budget` elapses: dirty (noted by a
/// producer) and never-measured entries first, then unfinished walks.
pub fn measure_some(
    ledger: &mut Ledger,
    budget: Duration,
    dirty: &[(PathBuf, String)],
    beat: &mut dyn FnMut(),
) -> (Vec<String>, Vec<String>) {
    let deadline = Instant::now() + budget;
    let mut measured = Vec::new();
    let mut resumable = Vec::new();
    let count = ledger.items.len();
    if count == 0 {
        return (measured, resumable);
    }
    let mut order: Vec<usize> = (0..count)
        .filter(|index| {
            let item = &ledger.items[*index];
            dirty
                .iter()
                .any(|(root, name)| *root == item.root && *name == item.name)
        })
        .collect();
    order.extend((0..count).filter(|index| ledger.items[*index].measured_at_ms == 0));
    order.extend((0..count).filter(|index| ledger.items[*index].walk.is_some()));
    order.extend((0..count).map(|offset| (ledger.cursor + offset) % count));
    let mut done = std::collections::BTreeSet::new();
    for index in order {
        if Instant::now() >= deadline {
            break;
        }
        if !done.insert(index) {
            continue;
        }
        let item = &mut ledger.items[index];
        let label = format!("{}:{}", root_kind_name(item.root_kind), item.name);
        if measure_item(item, deadline, beat) {
            measured.push(label);
        } else {
            resumable.push(label);
        }
        ledger.cursor = (index + 1) % count;
    }
    (measured, resumable)
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

/// A producer's note that it wrote `relative` under the root of `kind`
/// (cheap: one small file; the worker re-measures it first).
pub fn note_written(host_state: &Path, kind: RootKind, relative: &str) {
    if !check::enabled(host_state) {
        return;
    }
    let directory = host_state.join(DIRTY_DIRECTORY);
    let name = format!(
        "{:x}.json",
        <sha2::Sha256 as sha2::Digest>::digest(format!("{kind:?}:{relative}").as_bytes())
    );
    let written = fs::create_dir_all(&directory).and_then(|()| {
        serde_json::to_vec(&serde_json::json!({"kind": kind, "relative": relative}))
            .map_err(io::Error::other)
            .and_then(|bytes| write_atomically(&directory.join(name), &bytes))
    });
    crate::warn_unrecorded("note a storage write", written);
}

/// Take the producers' notes: `(root path, top-level entry name)` to
/// re-measure first.
pub(crate) fn take_dirty(host_state: &Path, roots: &[Root]) -> Vec<(PathBuf, String)> {
    let directory = host_state.join(DIRTY_DIRECTORY);
    let mut dirty = Vec::new();
    for name in read_names(&directory) {
        let path = directory.join(&name);
        let note: Option<serde_json::Value> = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        crate::warn_unrecorded("consume a storage write note", fs::remove_file(&path));
        let Some(note) = note else {
            continue;
        };
        let kind: Option<RootKind> = note
            .get("kind")
            .cloned()
            .and_then(|kind| serde_json::from_value(kind).ok());
        let top = note
            .get("relative")
            .and_then(serde_json::Value::as_str)
            .and_then(|relative| relative.split('/').next())
            .map(str::to_string);
        if let (Some(kind), Some(top)) = (kind, top) {
            for root in roots.iter().filter(|root| root.kind == kind) {
                dirty.push((root.path.clone(), top.clone()));
            }
        }
    }
    dirty
}

/// A gate cache entry's size from the ledger when measured within `fresh`,
/// for `gc plan`; read once per process.
pub(crate) fn fresh_gate_cache_size(path: &Path, fresh: Duration) -> Option<(u64, i64)> {
    static LEDGER: std::sync::OnceLock<Option<Ledger>> = std::sync::OnceLock::new();
    let ledger = LEDGER
        .get_or_init(|| {
            let host_state = crate::host_state::default_host_state_dir()?;
            check::enabled(&host_state).then(|| Ledger::load(&host_state))
        })
        .as_ref()?;
    let now = now_ms();
    for item in ledger
        .items
        .iter()
        .filter(|item| item.class == Class::GateCache && item.measured)
    {
        if now.saturating_sub(item.measured_at_ms)
            > u64::try_from(fresh.as_millis()).unwrap_or(u64::MAX)
        {
            continue;
        }
        let base = item.path();
        if let Ok(relative) = path.strip_prefix(&base)
            && let Some(child) = item
                .children
                .iter()
                .find(|child| Path::new(&child.name) == relative)
        {
            return Some((
                child.bytes,
                i64::try_from(item.measured_at_ms).unwrap_or(i64::MAX),
            ));
        }
    }
    None
}

impl crate::Broker {
    /// `broker status` advice from the storage ledger. Also registers this
    /// repository's configured worktree root (an exact path from its own
    /// configuration, never a guess) and its gate cache budget. Cheap: one
    /// small file read and a statvfs per root.
    pub(crate) fn storage_ledger_advice(&self) -> Vec<crate::StatusAdvice> {
        let Some(host_state) = self.storage_context().host_state_dir.clone() else {
            return Vec::new();
        };
        if !host_state.is_dir() || !check::enabled(&host_state) {
            return Vec::new();
        }
        let gate_storage = self.storage_context().gate_storage_locations();
        if let Some(location) = crate::gate_cache_gc::location_in(self.main_root(), &gate_storage)
            && let Ok(policy) = crate::retention::load_retention_policy(self.main_root())
        {
            check::register_gate_budget(
                &host_state,
                &location.repository_key,
                policy.gate_cache_bytes_budget,
            );
        }
        if let Ok(plan) = self.worktree_root_plan()
            && let Some(root) = plan.preferred_root
            && !root.starts_with(self.main_root())
        {
            check::register_root(
                &host_state,
                &root,
                RootKind::WorktreeRoot,
                Some(self.main_root()),
            );
        }
        cli::status_advice(&host_state)
    }
}
