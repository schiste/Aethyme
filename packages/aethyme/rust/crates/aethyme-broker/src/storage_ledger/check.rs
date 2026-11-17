//! The start-of-command check, the detached worker and its lock.
//!
//! Every broker command calls [`command_start`]: one `statvfs` per root and a
//! read of the small summary file, never a walk. Below a floor or over a
//! budget it starts the worker detached (at most once a minute) and, for
//! commands that need space now (`start`, preparation, gates), first removes
//! what the last pass already proved removable and refuses only if the
//! volume is still below its floor.
//!
//! The worker holds `<host state>/storage-worker.lock`, a record of its pid,
//! start time and heartbeat. A lock whose owner is gone (pid not running, or
//! running with another start time) or whose heartbeat stopped for longer
//! than [`STALE_HEARTBEAT`] is taken over; the old owner checks its token
//! before every removal and stops once it lost the lock.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::evict::{self, Candidate, Reason};
use super::{
    Budgets, Class, Ledger, PassReport, Root, RootKind, SUMMARY_FILE, Volume, now_ms,
    write_atomically,
};

pub const LOCK_FILE: &str = "storage-worker.lock";
/// Storage roots registered by repositories (`<host state>/storage-roots.json`).
pub const ROOTS_FILE: &str = "storage-roots.json";
/// Repositories' configured gate cache budgets
/// (`<host state>/storage-gate-budgets.json`).
pub const GATE_BUDGETS_FILE: &str = "storage-gate-budgets.json";
pub const STALE_HEARTBEAT: Duration = Duration::from_secs(15 * 60);
const KICK_EVERY: Duration = Duration::from_secs(60);
const REFRESH_EVERY: Duration = Duration::from_secs(10 * 60);
const MEASURE_BUDGET: Duration = Duration::from_secs(60);
const PACE: Duration = Duration::from_millis(200);

/// What a command reads at start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub updated_at_ms: u64,
    pub roots: Vec<Root>,
    pub over_budget: Vec<(String, u64, u64)>,
    pub unclassified_bytes: u64,
    pub unclassified_entries: u64,
    pub floor_bytes: u64,
    /// Removals the last pass proved safe but did not do (paced or within
    /// budget then), for a command that needs space now.
    pub reclaimable: Vec<Reclaimable>,
    pub unlanded_archives: Vec<String>,
    pub last_kick_ms: u64,
}

/// One removal a command may perform at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reclaimable {
    pub class: Class,
    pub root_kind: RootKind,
    pub root: PathBuf,
    pub relative: String,
    pub bytes: u64,
    pub newest_ms: u64,
}

impl Summary {
    pub fn load(host_state: &Path) -> Self {
        fs::read(host_state.join(SUMMARY_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, host_state: &Path) -> io::Result<()> {
        write_atomically(
            &host_state.join(SUMMARY_FILE),
            &serde_json::to_vec(self).map_err(io::Error::other)?,
        )
    }
}

/// The roots: host state, host cache, and registered storage roots.
pub fn roots(host_state: &Path, host_cache: Option<&Path>) -> Vec<Root> {
    let mut roots = vec![Root {
        path: host_state.to_path_buf(),
        kind: RootKind::HostState,
    }];
    if let Some(cache) = host_cache {
        roots.push(Root {
            path: cache.to_path_buf(),
            kind: RootKind::HostCache,
        });
    }
    let registered: Vec<PathBuf> = fs::read(host_state.join(ROOTS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    for path in registered {
        if path.is_dir() && !roots.iter().any(|root| root.path == path) {
            roots.push(Root {
                path,
                kind: RootKind::Storage,
            });
        }
    }
    roots
}

/// A storage root for a repository's worktree root or preparation cache:
/// its parent, when that parent is an Aethyme directory (so siblings that are
/// not Aethyme's are never inventoried).
pub fn storage_root_for(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let name = parent.file_name()?.to_str()?;
    name.to_ascii_lowercase()
        .contains("aethyme")
        .then(|| parent.to_path_buf())
}

/// Remember storage roots (cheap; a no-op when already known).
pub fn register_storage_roots(host_state: &Path, paths: &[PathBuf]) {
    let file = host_state.join(ROOTS_FILE);
    let mut known: Vec<PathBuf> = fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let before = known.len();
    for path in paths {
        if path != host_state && !known.contains(path) {
            known.push(path.clone());
        }
    }
    if known.len() != before {
        crate::warn_unrecorded(
            "register a storage root",
            serde_json::to_vec(&known)
                .map_err(io::Error::other)
                .and_then(|bytes| write_atomically(&file, &bytes)),
        );
    }
}

/// Remember a repository's configured gate cache budget (a no-op when
/// unchanged).
pub fn register_gate_budget(host_state: &Path, repository: &str, bytes: u64) {
    let file = host_state.join(GATE_BUDGETS_FILE);
    let mut known: BTreeMap<String, u64> = fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    if known.get(repository) == Some(&bytes) {
        return;
    }
    known.insert(repository.to_string(), bytes);
    crate::warn_unrecorded(
        "register a gate cache budget",
        serde_json::to_vec(&known)
            .map_err(io::Error::other)
            .and_then(|serialized| write_atomically(&file, &serialized)),
    );
}

/// Free and total bytes of the volume holding `path`.
pub fn statvfs(path: &Path) -> Option<(u64, u64)> {
    let free = crate::disk_headroom::available_bytes(path)?;
    let total = total_bytes(path).unwrap_or(0);
    Some((free, total))
}

#[cfg(unix)]
#[allow(clippy::unnecessary_cast, clippy::useless_conversion)]
fn total_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c_path` is a valid NUL-terminated string and `stat` is a
    // zeroed, writable statvfs the call fills in.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        Some(u64::from(stat.f_blocks).saturating_mul(u64::from(stat.f_frsize)))
    }
}

#[cfg(not(unix))]
fn total_bytes(_path: &Path) -> Option<u64> {
    None
}

/// What the start-of-command check decided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decision {
    /// The worker was (or should be) started.
    pub kick: bool,
    /// Roots whose volume is below the floor.
    pub below_floor: Vec<(RootKind, u64)>,
    /// Bytes removed at once for a command that needs space.
    pub reclaimed: u64,
    /// The refusal, when space is still short.
    pub refuse: Option<String>,
}

/// Whether a broker command needs free space now (and so may reclaim and
/// refuse).
pub fn needs_space(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("start" | "gate" | "gates") => true,
        Some("submit") => true,
        _ => args.iter().any(|arg| arg == "prepare"),
    }
}

/// The O(1) check: `free` reads a volume's free bytes, `kick` starts the
/// worker. Reads only the summary file.
pub fn decide(
    host_state: &Path,
    needs_space: bool,
    free: &mut dyn FnMut(&Path) -> Option<u64>,
    kick: &mut dyn FnMut(),
) -> Decision {
    let mut summary = Summary::load(host_state);
    let floor = if summary.floor_bytes == 0 {
        Budgets::default().free_floor_bytes
    } else {
        summary.floor_bytes
    };
    let mut decision = Decision::default();
    let roots = if summary.roots.is_empty() {
        vec![Root {
            path: host_state.to_path_buf(),
            kind: RootKind::HostState,
        }]
    } else {
        summary.roots.clone()
    };
    let mut seen = BTreeSet::new();
    for root in &roots {
        if let Some(available) = free(&root.path)
            && available < floor
            && seen.insert(root.kind)
        {
            decision.below_floor.push((root.kind, available));
        }
    }
    let now = now_ms();
    let stale = now.saturating_sub(summary.updated_at_ms) > duration_ms(REFRESH_EVERY);
    let wanted = stale || !decision.below_floor.is_empty() || !summary.over_budget.is_empty();
    let recently_kicked = now.saturating_sub(summary.last_kick_ms) < duration_ms(KICK_EVERY);
    if wanted && (!recently_kicked || !decision.below_floor.is_empty()) {
        kick();
        decision.kick = true;
        summary.last_kick_ms = now;
        crate::warn_unrecorded("record a storage worker start", summary.save(host_state));
    }
    if !needs_space || decision.below_floor.is_empty() {
        return decision;
    }
    // Space is needed now: remove what the last pass proved removable,
    // regenerable classes first, until every volume is above its floor.
    let registry = host_state.join("host-resources.db");
    let leases = evict::leased_gate_caches(&registry);
    let mut reclaimable = summary.reclaimable.clone();
    reclaimable.sort_by_key(|entry| (entry.class == Class::RecoveryArchives, entry.newest_ms));
    for entry in reclaimable {
        let short = roots.iter().any(|root| {
            free(&root.path).is_some_and(|available| available < floor)
                && root.kind == entry.root_kind
        });
        if !short {
            continue;
        }
        let candidate = Candidate {
            class: entry.class,
            root_kind: entry.root_kind,
            root: entry.root.clone(),
            relative: entry.relative.clone(),
            bytes: entry.bytes,
            newest_ms: entry.newest_ms,
            reason: Reason::OverBudget,
        };
        if evict::refuse(&candidate, leases.as_ref()).is_none()
            && evict::remove_protected(&candidate, &registry).is_ok()
        {
            decision.reclaimed += entry.bytes;
        }
    }
    let still_short: Vec<String> = roots
        .iter()
        .filter_map(|root| {
            free(&root.path)
                .filter(|available| *available < floor)
                .map(|available| {
                    format!(
                        "{} has {} free, below the {} floor",
                        super::root_kind_name(root.kind),
                        human(available),
                        human(floor)
                    )
                })
        })
        .collect();
    if !still_short.is_empty() {
        decision.refuse = Some(format!(
            "not enough free disk space: {} (removed {} of regenerable data first). Free space \
             on that volume, or see `aethyme storage status` for what Aethyme holds and why it \
             is kept",
            still_short.join("; "),
            human(decision.reclaimed)
        ));
    }
    decision
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The check every broker command runs first. `None` to continue; an exit
/// code after a refusal.
pub fn command_start(args: &[String]) -> Option<u8> {
    let host_state = crate::host_state::default_host_state_dir()?;
    if !host_state.is_dir() || !enabled(&host_state) {
        return None;
    }
    let decision = decide(
        &host_state,
        needs_space(args),
        &mut |path| crate::disk_headroom::available_bytes(path),
        &mut || crate::warn_unrecorded("start the storage worker", spawn_worker()),
    );
    let refusal = decision.refuse?;
    eprintln!("Error: {refusal}");
    Some(crate::exit_status::ENVIRONMENT)
}

/// The variable that turns the ledger on (`1`) or off (`0`); by default it
/// is on except for a host state directory under the system temporary
/// directory (test fixtures and scratch runs).
pub const ENABLE_ENV: &str = "AETHYME_STORAGE_LEDGER";

/// Whether this host state directory gets the start-of-command check,
/// worker starts and root registration.
pub fn enabled(host_state: &Path) -> bool {
    match std::env::var(ENABLE_ENV).as_deref() {
        Ok("0") => false,
        Ok("1") => true,
        _ => !crate::host_state::path_is_ephemeral(host_state),
    }
}

/// Start `aethyme storage worker` detached, in its own session. Only the
/// `aethyme` executable starts it (never a test harness or another binary
/// linking the broker).
pub fn spawn_worker() -> io::Result<()> {
    let executable = std::env::current_exe()?;
    if executable.file_name().and_then(|name| name.to_str()) != Some("aethyme") {
        return Ok(());
    }
    let mut command = std::process::Command::new(executable);
    command
        .args(["storage", "worker"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn()?;
    // Not waited for: it outlives this command. Its exit is reaped by init
    // once this process ends.
    std::mem::forget(child);
    Ok(())
}

/// The worker lock record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockRecord {
    pub pid: u32,
    pub started: String,
    pub heartbeat_ms: u64,
    pub token: String,
}

/// Whether a recorded process is still the one recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Alive,
    Gone,
    Unknown,
}

pub fn owner_state(pid: u32, started: &str) -> Owner {
    let Ok(target) = libc::pid_t::try_from(pid) else {
        return Owner::Unknown;
    };
    if target <= 1 || started.is_empty() {
        return Owner::Unknown;
    }
    // SAFETY: kill with signal 0 sends nothing; it only checks existence.
    let probed = unsafe { libc::kill(target, 0) };
    if probed != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        return Owner::Gone;
    }
    match crate::mcp::hub::process_start(pid) {
        Some(current) if current == started => Owner::Alive,
        Some(_) => Owner::Gone,
        None => Owner::Unknown,
    }
}

/// A held worker lock.
pub struct WorkerLock {
    path: PathBuf,
    record: LockRecord,
}

impl WorkerLock {
    /// Take the lock, taking over one whose owner is gone or whose heartbeat
    /// is stale. `Err` names who holds it.
    pub fn acquire(host_state: &Path) -> Result<Self, String> {
        let path = host_state.join(LOCK_FILE);
        let pid = std::process::id();
        let record = LockRecord {
            pid,
            started: crate::mcp::hub::process_start(pid).unwrap_or_default(),
            heartbeat_ms: now_ms(),
            token: format!("{pid}-{}", now_ms()),
        };
        for _ in 0..3 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    let bytes = serde_json::to_vec(&record).map_err(|error| error.to_string())?;
                    io::Write::write_all(&mut file, &bytes).map_err(|error| error.to_string())?;
                    return Ok(Self { path, record });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let held: Option<LockRecord> = fs::read(&path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                    let takeover = match &held {
                        None => true,
                        Some(held) => {
                            owner_state(held.pid, &held.started) == Owner::Gone
                                || now_ms().saturating_sub(held.heartbeat_ms)
                                    > duration_ms(STALE_HEARTBEAT)
                        }
                    };
                    if !takeover {
                        let held = held.expect("checked above");
                        return Err(format!("worker pid {} holds the storage lock", held.pid));
                    }
                    // Remove the stale record and race for a fresh one.
                    crate::warn_unrecorded(
                        "take over the storage worker lock",
                        fs::remove_file(&path),
                    );
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        Err("the storage worker lock changed hands while taking it over".into())
    }

    /// Refresh the heartbeat; false when the lock was taken over.
    pub fn beat(&mut self) -> bool {
        if !self.held() {
            return false;
        }
        self.record.heartbeat_ms = now_ms();
        serde_json::to_vec(&self.record)
            .map_err(io::Error::other)
            .and_then(|bytes| write_atomically(&self.path, &bytes))
            .is_ok()
    }

    /// Whether the lock is still this worker's.
    pub fn held(&self) -> bool {
        fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<LockRecord>(&bytes).ok())
            .is_some_and(|record| record.token == self.record.token)
    }
}

impl Drop for WorkerLock {
    fn drop(&mut self) {
        if self.held() {
            crate::warn_unrecorded(
                "release the storage worker lock",
                fs::remove_file(&self.path),
            );
        }
    }
}

/// Inputs of one pass, so tests can run it on temporary roots.
pub struct PassInputs<'a> {
    pub host_state: &'a Path,
    pub roots: Vec<Root>,
    pub budgets: Budgets,
    pub free: &'a mut dyn FnMut(&Path) -> Option<(u64, u64)>,
    pub measure_budget: Duration,
    pub pace: Duration,
    /// The lease registry for gate caches.
    pub registry: PathBuf,
}

/// One worker pass: volumes, discovery, measurement, eviction, persistence.
pub fn run_pass(inputs: PassInputs<'_>, lock: Option<&mut WorkerLock>) -> io::Result<Ledger> {
    let started = Instant::now();
    let mut report = PassReport {
        started_at_ms: now_ms(),
        ..PassReport::default()
    };
    let mut ledger = Ledger::load(inputs.host_state);
    ledger.roots = inputs.roots.clone();
    for root in &ledger.roots {
        evict::sweep_aside(&root.path);
    }
    let mut phase = Instant::now();
    super::discover(&mut ledger);
    report.phases_ms.insert("discover".into(), elapsed(phase));
    phase = Instant::now();
    report.measured = super::measure_some(&mut ledger, inputs.measure_budget);
    report.phases_ms.insert("measure".into(), elapsed(phase));

    // Volumes and pressure.
    phase = Instant::now();
    let floor = inputs.budgets.free_floor_bytes;
    let mut aggressive = BTreeSet::new();
    let mut volumes: BTreeMap<u64, Volume> = BTreeMap::new();
    for root in &ledger.roots {
        let Some(device) = super::device_of(&root.path) else {
            continue;
        };
        let Some((free, total)) = (inputs.free)(&root.path) else {
            continue;
        };
        let breaches = ledger
            .volumes
            .iter()
            .find(|volume| volume.device == device)
            .map_or(0, |volume| volume.floor_breaches);
        let volume = volumes.entry(device).or_insert(Volume {
            device,
            free_bytes: free,
            total_bytes: total,
            measured_at_ms: now_ms(),
            floor_breaches: breaches,
        });
        if free < floor {
            aggressive.insert(root.kind);
        }
        volume.free_bytes = free;
    }
    for volume in volumes.values_mut() {
        if volume.free_bytes < floor {
            volume.floor_breaches += 1;
        }
    }
    for (kind, bytes) in ledger.root_bytes() {
        if bytes > inputs.budgets.root_total(kind) {
            aggressive.insert(kind);
        }
    }
    report.pressure = !aggressive.is_empty();
    report.phases_ms.insert("volumes".into(), elapsed(phase));

    // Eviction.
    phase = Instant::now();
    let now = now_ms();
    let candidates = evict::plan(&ledger, &inputs.budgets, now, &aggressive);
    let leases = evict::leased_gate_caches(&inputs.registry);
    let mut class_bytes = ledger.class_bytes();
    let mut gate_bytes = evict::gate_cache_by_repository(&ledger);
    let mut root_bytes = ledger.root_bytes();
    let mut lock = lock;
    for candidate in candidates {
        let still_needed = match candidate.reason {
            Reason::Expired => true,
            Reason::OverBudget => {
                aggressive.contains(&candidate.root_kind)
                    || root_bytes.get(&candidate.root_kind).copied().unwrap_or(0)
                        > inputs.budgets.root_total(candidate.root_kind)
                    || match candidate.class {
                        Class::GateCache => {
                            let repository = candidate.relative.split('/').nth(1).unwrap_or("");
                            gate_bytes.get(repository).copied().unwrap_or(0)
                                > inputs.budgets.gate_budget(repository)
                        }
                        class => inputs.budgets.class_bytes(class).is_some_and(|budget| {
                            class_bytes.get(&class).copied().unwrap_or(0) > budget
                        }),
                    }
            }
        };
        if !still_needed {
            continue;
        }
        if let Some(lock) = lock.as_deref_mut()
            && !lock.beat()
        {
            report
                .skipped
                .push("the storage worker lock was taken over".into());
            break;
        }
        if let Some(why) = evict::refuse(&candidate, leases.as_ref()) {
            if candidate.class == Class::RecoveryArchives && why.starts_with("unlanded") {
                report.unlanded_archives.push(candidate.relative.clone());
            }
            report.skipped.push(format!("{}: {why}", candidate.label()));
            continue;
        }
        match evict::remove_protected(&candidate, &inputs.registry) {
            Ok(()) => {
                report.evicted.push(candidate.label());
                report.bytes_reclaimed += candidate.bytes;
                let telemetry = ledger.telemetry.entry(candidate.class).or_default();
                telemetry.evictions += 1;
                telemetry.bytes_reclaimed += candidate.bytes;
                for bytes in [
                    class_bytes.entry(candidate.class).or_insert(0),
                    root_bytes.entry(candidate.root_kind).or_insert(0),
                ] {
                    *bytes = bytes.saturating_sub(candidate.bytes);
                }
                if candidate.class == Class::GateCache {
                    let repository = candidate.relative.split('/').nth(1).unwrap_or("");
                    if let Some(bytes) = gate_bytes.get_mut(repository) {
                        *bytes = bytes.saturating_sub(candidate.bytes);
                    }
                }
                forget(&mut ledger, &candidate);
            }
            Err(error) => report
                .skipped
                .push(format!("{}: {error}", candidate.label())),
        }
        // Paced, except under pressure.
        if !report.pressure && !inputs.pace.is_zero() {
            std::thread::sleep(inputs.pace);
        }
    }
    report.phases_ms.insert("evict".into(), elapsed(phase));
    report.phases_ms.insert("total".into(), elapsed(started));

    ledger.volumes = volumes.into_values().collect();
    super::update_over_budget_clock(&mut ledger, &inputs.budgets, now_ms());
    ledger.updated_at_ms = now_ms();
    ledger.last_pass = Some(report.clone());
    ledger.save(inputs.host_state)?;

    let previous = Summary::load(inputs.host_state);
    let unclassified: Vec<&super::Item> = ledger
        .items
        .iter()
        .filter(|item| item.class == Class::Unclassified)
        .collect();
    let summary = Summary {
        updated_at_ms: ledger.updated_at_ms,
        roots: ledger.roots.clone(),
        over_budget: ledger.over_budget(&inputs.budgets),
        unclassified_bytes: unclassified.iter().map(|item| item.bytes).sum(),
        unclassified_entries: unclassified.len() as u64,
        floor_bytes: floor,
        // What a command short of space may remove at once: regenerable
        // data only (never an archive), still re-checked before removal.
        reclaimable: evict::plan(
            &ledger,
            &inputs.budgets,
            now_ms(),
            &[RootKind::HostState, RootKind::HostCache, RootKind::Storage]
                .into_iter()
                .collect(),
        )
        .into_iter()
        .filter(|candidate| candidate.class != Class::RecoveryArchives)
        .map(|candidate| Reclaimable {
            class: candidate.class,
            root_kind: candidate.root_kind,
            root: candidate.root,
            relative: candidate.relative,
            bytes: candidate.bytes,
            newest_ms: candidate.newest_ms,
        })
        .collect(),
        unlanded_archives: report.unlanded_archives.clone(),
        last_kick_ms: previous.last_kick_ms,
    };
    summary.save(inputs.host_state)?;
    Ok(ledger)
}

/// Drop a removed entry or child from the ledger.
fn forget(ledger: &mut Ledger, candidate: &Candidate) {
    let (name, child) = match candidate.relative.split_once('/') {
        Some((name, child)) => (name, Some(child)),
        None => (candidate.relative.as_str(), None),
    };
    for item in &mut ledger.items {
        if item.root != candidate.root || item.name != name {
            continue;
        }
        match child {
            None => item.bytes = 0,
            Some(child) => {
                item.children.retain(|entry| entry.name != child);
                item.bytes = item.bytes.saturating_sub(candidate.bytes);
            }
        }
    }
    if child.is_none() {
        ledger
            .items
            .retain(|item| !(item.root == candidate.root && item.name == name));
    }
}

fn elapsed(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The detached worker: take the lock and run one pass over this host.
pub fn run_worker_pass() -> Result<Ledger, String> {
    let host_state = crate::host_state::default_host_state_dir()
        .ok_or("the host state directory cannot be resolved")?;
    let host_cache = crate::host_state::default_host_cache_dir();
    let mut lock = WorkerLock::acquire(&host_state)?;
    let (budgets, _) = Budgets::load(&host_state);
    let inputs = PassInputs {
        host_state: &host_state,
        roots: roots(&host_state, host_cache.as_deref()),
        budgets,
        free: &mut statvfs,
        measure_budget: MEASURE_BUDGET,
        pace: PACE,
        registry: host_state.join("host-resources.db"),
    };
    run_pass(inputs, Some(&mut lock)).map_err(|error| error.to_string())
}
