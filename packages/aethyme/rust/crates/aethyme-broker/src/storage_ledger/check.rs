//! The start-of-command check, the detached worker and its lock.
//!
//! Every broker command calls [`command_start`]: one `statvfs` per root and a
//! read of two small files (the summary and the worker lock), never a walk
//! and never `ps`. Over a budget or below a floor it starts the worker
//! detached, at most once a minute and never while a worker is alive. Only
//! commands on an explicit list of writers (`start`, a bare `submit`, a bare
//! `submit prepare`) may refuse, only for the volumes they write to, and
//! only below the hard minimum after a bounded reclaim (time and bytes)
//! failed; between the minimum and the floor they proceed and advice warns.
//!
//! The worker holds `<host state>/storage-worker.lock`, a record of its pid,
//! start time and heartbeat. Every walk and deletion refreshes the
//! heartbeat at least every second, so a lock is taken over only when its
//! owner is gone (pid not running, or running with another start time) or
//! its heartbeat stopped for [`STALE_HEARTBEAT`], far longer than any step
//! between two refreshes; the old owner checks its token at every refresh
//! and stops once it lost the lock, so two workers never walk at once.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::evict::{self, Candidate, Reason, duration_ms};
use super::{
    Budgets, Class, Ledger, PassReport, Root, RootKind, STORAGE_ROOT_MARKER, SUMMARY_FILE, Volume,
    now_ms, write_atomically,
};

pub const LOCK_FILE: &str = "storage-worker.lock";
/// Roots registered by repositories and adoptions (`<host state>/storage-roots.json`).
pub const ROOTS_FILE: &str = "storage-roots.json";
/// Repositories' configured gate cache budgets
/// (`<host state>/storage-gate-budgets.json`).
pub const GATE_BUDGETS_FILE: &str = "storage-gate-budgets.json";
pub const STALE_HEARTBEAT: Duration = Duration::from_secs(15 * 60);
const KICK_EVERY: Duration = Duration::from_secs(60);
const REFRESH_EVERY: Duration = Duration::from_secs(10 * 60);
const MEASURE_BUDGET: Duration = Duration::from_secs(60);
const PACE: Duration = Duration::from_millis(200);
/// Reclaim at command start stops after this long.
pub const RECLAIM_TIME: Duration = Duration::from_millis(500);

/// What a command reads at start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    pub updated_at_ms: u64,
    pub roots: Vec<Root>,
    pub over_budget: Vec<(String, u64, u64)>,
    pub unclassified_bytes: u64,
    pub unclassified_entries: u64,
    pub floor_bytes: u64,
    #[serde(default)]
    pub hard_minimum_bytes: u64,
    /// Removals the last pass found eligible, for a command short of space;
    /// each is re-checked before removal.
    pub reclaimable: Vec<Reclaimable>,
    pub unlanded_archives: Vec<String>,
    pub last_kick_ms: u64,
    /// Archives that expire within the notice week, soonest first (the
    /// first 20), and how many there are.
    #[serde(default)]
    pub archives_expiring: Vec<ExpiringArchive>,
    #[serde(default)]
    pub archives_expiring_total: u64,
    /// Worker starts skipped because no installed binary of this build was
    /// found (only a temporary copy runs).
    #[serde(default)]
    pub worker_spawns_skipped: u64,
}

/// One archive about to expire, as advice shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpiringArchive {
    /// `<pool>/<archive>`, as `storage keep-archive` takes it.
    pub id: String,
    /// The repository pool (the archive's parent directory name).
    pub pool: String,
    pub session_id: Option<i64>,
    pub bytes: u64,
    pub expires_ms: u64,
    /// When this expiry was first announced; removal waits a week from it.
    pub noticed_at_ms: u64,
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

/// A registered root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Registered {
    path: PathBuf,
    kind: RootKind,
    #[serde(default)]
    repositories: Vec<PathBuf>,
}

fn registered(host_state: &Path) -> Vec<Registered> {
    fs::read(host_state.join(ROOTS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// The roots: host state and host cache (exact), registered worktree roots
/// (exact configured paths), and storage roots that carry the marker.
pub fn roots(host_state: &Path, host_cache: Option<&Path>) -> Vec<Root> {
    let mut roots = vec![Root::new(host_state, RootKind::HostState)];
    if let Some(cache) = host_cache.filter(|cache| cache.is_dir()) {
        roots.push(Root::new(cache, RootKind::HostCache));
    }
    for entry in registered(host_state) {
        let usable = entry.path.is_dir()
            && !roots.iter().any(|root| root.path == entry.path)
            && match entry.kind {
                RootKind::Storage => entry.path.join(STORAGE_ROOT_MARKER).is_file(),
                RootKind::WorktreeRoot => true,
                RootKind::HostState | RootKind::HostCache => false,
            };
        if usable {
            roots.push(Root {
                path: entry.path,
                kind: entry.kind,
                repositories: entry.repositories,
            });
        }
    }
    roots
}

/// Remember an exact root, and for a worktree root the repository whose
/// root it is (a no-op when already known).
pub fn register_root(host_state: &Path, path: &Path, kind: RootKind, repository: Option<&Path>) {
    let file = host_state.join(ROOTS_FILE);
    let mut known = registered(host_state);
    if path == host_state {
        return;
    }
    match known.iter_mut().find(|entry| entry.path == path) {
        Some(entry) => match repository {
            Some(repository) if !entry.repositories.iter().any(|known| known == repository) => {
                entry.repositories.push(repository.to_path_buf());
            }
            _ => return,
        },
        None => known.push(Registered {
            path: path.to_path_buf(),
            kind,
            repositories: repository.map(Path::to_path_buf).into_iter().collect(),
        }),
    }
    crate::warn_unrecorded(
        "register a storage root",
        serde_json::to_vec(&known)
            .map_err(io::Error::other)
            .and_then(|bytes| write_atomically(&file, &bytes)),
    );
}

/// Adopt a directory as a storage root: write the marker and register it.
/// Refused for a repository's `.aethyme/`, a repository checkout, or a
/// path inside one.
pub fn adopt(host_state: &Path, path: &Path) -> Result<PathBuf, String> {
    let path = fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !path.is_dir() {
        return Err(format!("{} is not a directory", path.display()));
    }
    if path.ancestors().any(|ancestor| {
        ancestor.file_name().is_some_and(|name| name == ".aethyme")
            || ancestor.join(".git").exists()
    }) {
        return Err(format!(
            "{} is a repository or inside one; a repository's `.aethyme/` is never a storage root",
            path.display()
        ));
    }
    fs::write(
        path.join(STORAGE_ROOT_MARKER),
        "This directory is an Aethyme storage root (`aethyme storage adopt`).\n",
    )
    .map_err(|error| error.to_string())?;
    register_root(host_state, &path, RootKind::Storage, None);
    Ok(path)
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
    /// The worker was started.
    pub kick: bool,
    /// Roots whose volume is below the floor (warn) or the hard minimum.
    pub below_floor: Vec<(RootKind, u64)>,
    /// Bytes removed at once for a command that needs space.
    pub reclaimed: u64,
    /// The refusal, when space is still short.
    pub refuse: Option<String>,
}

/// The volumes a broker command writes to, from an explicit list of
/// writers: `start` (the worktree root), a bare `submit` (gates: the host
/// cache), a bare `submit prepare` (preparation: storage roots). Every other
/// command, `submit prepare status` included, writes none and never refuses.
pub fn writes_to(args: &[String]) -> Vec<RootKind> {
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .take_while(|arg| !arg.starts_with('-'))
        .collect();
    match words.as_slice() {
        ["start"] => vec![RootKind::WorktreeRoot, RootKind::HostState],
        ["submit"] => vec![RootKind::HostCache],
        ["submit", "prepare"] => vec![RootKind::Storage, RootKind::HostState],
        _ => Vec::new(),
    }
}

/// Whether a live worker holds the lock (read from the lock file only).
fn worker_alive(host_state: &Path) -> bool {
    fs::read(host_state.join(LOCK_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<LockRecord>(&bytes).ok())
        .is_some_and(|record| {
            now_ms().saturating_sub(record.heartbeat_ms) <= duration_ms(STALE_HEARTBEAT)
        })
}

/// The O(1) check: `free` reads a volume's free bytes, `kick` starts the
/// worker. Reads only the summary and lock files.
pub fn decide(
    host_state: &Path,
    writes: &[RootKind],
    cwd: Option<&Path>,
    free: &mut dyn FnMut(&Path) -> Option<u64>,
    kick: &mut dyn FnMut(),
) -> Decision {
    let mut summary = Summary::load(host_state);
    let defaults = Budgets::default();
    let floor = if summary.floor_bytes == 0 {
        defaults.free_floor_bytes
    } else {
        summary.floor_bytes
    };
    let minimum = if summary.hard_minimum_bytes == 0 {
        defaults.hard_minimum_bytes
    } else {
        summary.hard_minimum_bytes
    };
    let mut decision = Decision::default();
    let roots = if summary.roots.is_empty() {
        vec![Root::new(host_state, RootKind::HostState)]
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
    // One spawn a minute at most, and never while a worker runs.
    let recently_kicked = now.saturating_sub(summary.last_kick_ms) < duration_ms(KICK_EVERY);
    if wanted && !recently_kicked && !worker_alive(host_state) {
        kick();
        decision.kick = true;
        summary.last_kick_ms = now;
        crate::warn_unrecorded("record a storage worker start", summary.save(host_state));
    }
    // Only a writer, only for the volumes it writes (its own repository's
    // worktree root, not another's), only below the minimum.
    let written: Vec<&Root> = roots
        .iter()
        .filter(|root| writes.contains(&root.kind) && root.written_from(cwd))
        .collect();
    let short = |free: &mut dyn FnMut(&Path) -> Option<u64>, root: &Root| {
        free(&root.path).is_some_and(|available| available < minimum)
    };
    if !written.iter().any(|root| short(free, root)) {
        return decision;
    }
    // Bounded reclaim: regenerable data the last pass found, oldest first,
    // for at most RECLAIM_TIME and only on the short volumes.
    let started = Instant::now();
    let registry = host_state.join("host-resources.db");
    let leases = evict::leased_gate_caches(&registry);
    let mut reclaimable = summary.reclaimable.clone();
    reclaimable.sort_by_key(|entry| entry.newest_ms);
    for entry in reclaimable {
        if started.elapsed() >= RECLAIM_TIME {
            break;
        }
        let Some(root) = roots.iter().find(|root| root.path == entry.root) else {
            continue;
        };
        if super::device_of(&root.path).is_none() || !short(free, root) {
            continue;
        }
        let shares_volume = written
            .iter()
            .any(|written| super::device_of(&written.path) == super::device_of(&root.path));
        if !shares_volume {
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
            expires_ms: None,
        };
        if evict::refuse(&candidate, leases.as_ref(), now).is_some() {
            continue;
        }
        // The deadline holds inside the deletion too: an interrupted one
        // leaves its renamed remainder, which every worker pass sweeps.
        match evict::remove_protected(
            &candidate,
            &registry,
            &mut || {},
            Some(started + RECLAIM_TIME),
        ) {
            Ok(true) => decision.reclaimed += entry.bytes,
            Ok(false) => break,
            Err(_) => {}
        }
    }
    let still_short: Vec<String> = written
        .iter()
        .filter_map(|root| {
            free(&root.path)
                .filter(|available| *available < minimum)
                .map(|available| {
                    format!(
                        "the {} volume has {} free, below the {} minimum",
                        super::root_kind_name(root.kind),
                        human(available),
                        human(minimum)
                    )
                })
        })
        .collect();
    if !still_short.is_empty() {
        decision.refuse = Some(format!(
            "not enough free disk space for this command: {} (removed {} of regenerable data \
             first; the storage worker continues). Free space on that volume, lower \
             `hard_minimum_bytes` in <host state>/storage.toml [budget], or set \
             AETHYME_STORAGE_LEDGER=0 to skip this check; `aethyme storage status` shows what \
             Aethyme holds",
            still_short.join("; "),
            human(decision.reclaimed)
        ));
    }
    decision
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

/// The check every broker command runs first. `None` to continue; an exit
/// code after a refusal.
pub fn command_start(args: &[String]) -> Option<u8> {
    let host_state = crate::host_state::default_host_state_dir()?;
    if !host_state.is_dir() || !enabled(&host_state) {
        return None;
    }
    let executable = std::env::current_exe().ok();
    if !start_check_allowed(args, executable.as_deref()) {
        return None;
    }
    let cwd = std::env::current_dir().ok();
    let decision = decide(
        &host_state,
        &writes_to(args),
        cwd.as_deref(),
        &mut |path| crate::disk_headroom::available_bytes(path),
        &mut || crate::warn_unrecorded("start the storage worker", spawn_worker()),
    );
    let refusal = decision.refuse?;
    eprintln!("Error: {refusal}");
    Some(crate::exit_status::ENVIRONMENT)
}

/// Whether `path` is where `update apply` stages a payload:
/// `<temp>/<.tmpXXXX>/payload/aethyme`.
pub fn update_payload(path: &Path) -> bool {
    let temp = std::env::temp_dir();
    let canonical_temp = fs::canonicalize(&temp).unwrap_or_else(|_| temp.clone());
    let staging = path.parent();
    let tempdir = staging.and_then(Path::parent);
    let named = staging
        .and_then(Path::file_name)
        .is_some_and(|name| name == crate::update::PAYLOAD_DIRECTORY)
        && tempdir
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(".tmp"));
    named
        && tempdir.and_then(Path::parent).is_some_and(|parent| {
            parent == temp
                || parent == canonical_temp
                || fs::canonicalize(parent).is_ok_and(|parent| parent == canonical_temp)
        })
}

/// Whether an executable is a temporary copy, not the installed binary: it
/// lies under the system temporary directory (the self-update's payload
/// staging, [`update_payload`], included).
pub fn ephemeral_executable(path: &Path) -> bool {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let temp = std::env::temp_dir();
    let canonical_temp = fs::canonicalize(&temp).unwrap_or_else(|_| temp.clone());
    update_payload(path) || canonical.starts_with(&canonical_temp) || path.starts_with(&temp)
}

/// Whether the start-of-command check may run at all (it may start the
/// worker or remove data inline): never from a temporary copy of the binary
/// such as the self-update's staged payload, and never for that payload's
/// verification (`broker quick-test`), which also sets [`ENABLE_ENV`]=`0`.
pub fn start_check_allowed(args: &[String], executable: Option<&Path>) -> bool {
    if args.first().map(String::as_str) == Some("quick-test") {
        return false;
    }
    executable.is_some_and(|executable| !ephemeral_executable(executable))
}

/// Where an installed `aethyme` usually is, in order.
pub fn install_locations(home: Option<&Path>) -> Vec<PathBuf> {
    let mut locations = Vec::new();
    if let Some(home) = home {
        locations.push(home.join(".local/bin/aethyme"));
        locations.push(home.join(".cargo/bin/aethyme"));
    }
    locations.push(PathBuf::from("/opt/homebrew/bin/aethyme"));
    locations.push(PathBuf::from("/usr/local/bin/aethyme"));
    locations
}

/// The binary the worker is started from: this executable when it is a
/// real install; otherwise the first known install location that is the
/// same file or reports exactly this build (`same_build`); `None` when none
/// qualifies (never the first `aethyme` on PATH, never a temporary copy).
pub fn worker_executable(
    current: &Path,
    candidates: &[PathBuf],
    same_build: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let named = current.file_name().and_then(|name| name.to_str()) == Some("aethyme");
    if named && !ephemeral_executable(current) {
        return Some(current.to_path_buf());
    }
    let current_canonical = fs::canonicalize(current).ok();
    candidates
        .iter()
        .filter(|candidate| candidate.is_file() && !ephemeral_executable(candidate))
        .find(|candidate| {
            fs::canonicalize(candidate)
                .ok()
                .is_some_and(|canonical| Some(&canonical) == current_canonical.as_ref())
                || same_build(candidate)
        })
        .cloned()
}

/// Whether `candidate --version` reports exactly this build.
fn reports_this_build(candidate: &Path) -> bool {
    let build = crate::version::current_binary_build();
    let output = std::process::Command::new(candidate)
        .arg("--version")
        .env(ENABLE_ENV, "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(output) = output else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    output.status.success()
        && text.split_whitespace().nth(1) == Some(build.version.as_str())
        && build
            .commit
            .as_deref()
            .is_none_or(|commit| text.contains(commit))
}

/// Start `aethyme storage worker` detached, in its own process group, from
/// the installed binary ([`worker_executable`]); nothing when only a test
/// harness, another binary linking the broker, or a temporary copy runs.
pub fn spawn_worker() -> io::Result<()> {
    let current = std::env::current_exe()?;
    if current.file_name().and_then(|name| name.to_str()) != Some("aethyme") {
        return Ok(());
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let Some(executable) = worker_executable(
        &current,
        &install_locations(home.as_deref()),
        &reports_this_build,
    ) else {
        // Counted: the summary records the start that did not happen.
        if let Some(host_state) = crate::host_state::default_host_state_dir() {
            let mut summary = Summary::load(&host_state);
            summary.worker_spawns_skipped += 1;
            crate::warn_unrecorded("count a skipped worker start", summary.save(&host_state));
        }
        return Ok(());
    };
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
    // Not waited for: it outlives this command.
    std::mem::forget(child);
    Ok(())
}

/// Lower this process's CPU and I/O priority (best effort).
pub fn lower_priority() {
    #[cfg(unix)]
    {
        // SAFETY: setpriority takes no pointers; it changes this process's
        // nice value.
        let _niced = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10) };
    }
    #[cfg(target_os = "linux")]
    {
        // ioprio_set(IOPRIO_WHO_PROCESS = 1, self, IOPRIO_CLASS_IDLE = 3 << 13).
        // SAFETY: a raw syscall with integer arguments only.
        let _idle = unsafe { libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13) };
    }
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn setiopolicy_np(
                iotype: libc::c_int,
                scope: libc::c_int,
                policy: libc::c_int,
            ) -> libc::c_int;
        }
        // IOPOL_TYPE_DISK = 0, IOPOL_SCOPE_PROCESS = 0, IOPOL_THROTTLE = 3
        // (<sys/resource.h>).
        // SAFETY: integer arguments only; it changes this process's policy.
        let _throttled = unsafe { setiopolicy_np(0, 0, 3) };
    }
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
    lost: bool,
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
                    return Ok(Self {
                        path,
                        record,
                        lost: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let held: Option<LockRecord> = fs::read(&path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
                    if let Some(held) = &held {
                        let gone = owner_state(held.pid, &held.started) == Owner::Gone;
                        let stalled = now_ms().saturating_sub(held.heartbeat_ms)
                            > duration_ms(STALE_HEARTBEAT);
                        if !gone && !stalled {
                            return Err(format!("worker pid {} holds the storage lock", held.pid));
                        }
                    }
                    // Remove the dead or stalled record and race for a fresh one.
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

    /// Refresh the heartbeat; false (and from then on) when the lock was
    /// taken over.
    pub fn beat(&mut self) -> bool {
        if self.lost || !self.held() {
            self.lost = true;
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

    pub fn lost(&self) -> bool {
        self.lost
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
    let mut lock = lock;
    let mut beat = || {
        if let Some(lock) = lock.as_deref_mut() {
            lock.beat();
        }
    };
    let mut ledger = Ledger::load(inputs.host_state);
    ledger.roots = inputs.roots.clone();
    for root in &ledger.roots {
        evict::sweep_aside(&root.path, &mut beat);
    }
    let mut phase = Instant::now();
    super::discover(&mut ledger);
    report.phases_ms.insert("discover".into(), elapsed(phase));
    phase = Instant::now();
    let dirty = super::take_dirty(inputs.host_state, &ledger.roots);
    let (measured, resumable) =
        super::measure_some(&mut ledger, inputs.measure_budget, &dirty, &mut beat);
    report.measured = measured;
    report.resumable = resumable;
    // Clock sanity for age expiry: a clock that went back, or passes that
    // stopped for over a week (the machine was off through a notice week),
    // remove nothing by age now, and every pending notice starts again.
    let pass_now = now_ms();
    let last_pass = ledger.updated_at_ms;
    let week = evict::duration_ms(super::ARCHIVE_NOTICE);
    ledger.age_frozen = last_pass > 0 && (pass_now < last_pass || pass_now - last_pass > week);
    if ledger.age_frozen {
        for notice in ledger.archive_notices.values_mut() {
            *notice = super::Notice::new(pass_now);
        }
        report.skipped.push(
            "age expiry paused this pass: the clock went back or no pass ran for over a week; \
             notices restart"
                .into(),
        );
    }
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
    // Age expiry is capped per pass; the rest stay noticed.
    let (mut aged, mut aged_bytes) = (0usize, 0u64);
    for candidate in candidates {
        let age_expiry =
            candidate.class == Class::RecoveryArchives && candidate.reason == Reason::Expired;
        if age_expiry
            && (aged >= super::AGE_EXPIRY_PER_PASS
                || aged_bytes.saturating_add(candidate.bytes) > super::AGE_EXPIRY_BYTES_PER_PASS)
        {
            report.skipped.push(format!(
                "{}: over this pass's age-expiry cap",
                candidate.label()
            ));
            continue;
        }
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
        // A real re-read of the lock token before each removal.
        if lock
            .as_deref()
            .is_some_and(|lock| lock.lost() || !lock.held())
        {
            report
                .skipped
                .push("the storage worker lock was taken over".into());
            break;
        }
        if let Some(why) = evict::refuse(&candidate, leases.as_ref(), now) {
            if candidate.class == Class::RecoveryArchives && why.starts_with("unlanded") {
                report.unlanded_archives.push(candidate.relative.clone());
            }
            report.skipped.push(format!("{}: {why}", candidate.label()));
            continue;
        }
        let mut beat = || {
            if let Some(lock) = lock.as_deref_mut() {
                lock.beat();
            }
        };
        match evict::remove_protected(&candidate, &inputs.registry, &mut beat, None) {
            Ok(_) => {
                report.evicted.push(candidate.label());
                report.bytes_reclaimed += candidate.bytes;
                let telemetry = ledger.telemetry.entry(candidate.class).or_default();
                telemetry.evictions += 1;
                telemetry.bytes_reclaimed += candidate.bytes;
                if age_expiry {
                    aged += 1;
                    aged_bytes += candidate.bytes;
                    ledger.archive_telemetry.expired += 1;
                    ledger.archive_telemetry.expired_bytes += candidate.bytes;
                }
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

    // Archives expiring within the notice week, and what owners kept.
    let (budgets, notice_end) = (
        &inputs.budgets,
        now_ms().saturating_add(evict::duration_ms(super::ARCHIVE_NOTICE)),
    );
    let mut expiring: Vec<ExpiringArchive> = Vec::new();
    let mut telemetry = super::ArchiveTelemetry {
        expired: ledger.archive_telemetry.expired,
        expired_bytes: ledger.archive_telemetry.expired_bytes,
        ..super::ArchiveTelemetry::default()
    };
    let published_at = now_ms();
    let mut notices = std::mem::take(&mut ledger.archive_notices);
    let mut announced = BTreeMap::new();
    for item in ledger
        .items
        .iter()
        .filter(|item| item.class == Class::RecoveryArchives)
    {
        for child in &item.children {
            if child.kept {
                telemetry.kept += 1;
                telemetry.kept_bytes += child.bytes;
                continue;
            }
            if let Some(expires) = super::archive_expiry(child, budgets)
                && expires <= notice_end
            {
                // Published now: the notice starts, or counts one more pass.
                let mut notice = notices
                    .remove(&child.name)
                    .unwrap_or_else(|| super::Notice::new(published_at));
                notice.publish(published_at);
                telemetry.noticed += 1;
                telemetry.noticed_bytes += child.bytes;
                expiring.push(ExpiringArchive {
                    id: child.name.clone(),
                    pool: child.name.split('/').next().unwrap_or_default().to_string(),
                    session_id: child.session_id,
                    bytes: child.bytes,
                    expires_ms: expires,
                    noticed_at_ms: notice.noticed_at_ms,
                });
                announced.insert(child.name.clone(), notice);
            }
        }
    }
    // A notice for an archive no longer announced (removed, kept) lapses.
    ledger.archive_notices = announced;
    expiring.sort_by_key(|archive| archive.expires_ms);
    ledger.archive_telemetry = telemetry;
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
        unclassified_bytes: unclassified
            .iter()
            .filter(|item| item.measured)
            .map(|item| item.bytes)
            .sum(),
        unclassified_entries: unclassified.len() as u64,
        floor_bytes: floor,
        hard_minimum_bytes: inputs.budgets.hard_minimum_bytes,
        // What a command short of space may remove at once: regenerable
        // data only (never an archive), still re-checked before removal.
        reclaimable: evict::plan(
            &ledger,
            &inputs.budgets,
            now_ms(),
            &[
                RootKind::HostState,
                RootKind::HostCache,
                RootKind::WorktreeRoot,
                RootKind::Storage,
            ]
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
        worker_spawns_skipped: previous.worker_spawns_skipped,
        archives_expiring: expiring.iter().take(20).cloned().collect(),
        archives_expiring_total: expiring.len() as u64,
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
    match child {
        None => ledger
            .items
            .retain(|item| !(item.root == candidate.root && item.name == name)),
        Some(child) => {
            for item in &mut ledger.items {
                if item.root == candidate.root && item.name == name {
                    item.children.retain(|entry| entry.name != child);
                    item.bytes = item.bytes.saturating_sub(candidate.bytes);
                }
            }
        }
    }
}

fn elapsed(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The detached worker: lower its priority, take the lock and run one pass
/// over this host.
pub fn run_worker_pass() -> Result<Ledger, String> {
    let host_state = crate::host_state::default_host_state_dir()
        .ok_or("the host state directory cannot be resolved")?;
    let host_cache = crate::host_state::default_host_cache_dir();
    lower_priority();
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
