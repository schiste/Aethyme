//! Affected gate runner (Phase 4): `.aethyme/gates.toml` config,
//! glob-triggered selection, cheap-first execution with a tree-hash
//! result cache, and cancellation of runs superseded by newer trees.
//!
//! Config format:
//!
//! ```toml
//! [[gate]]
//! name = "cargo-test"
//! command = "cargo test --workspace"
//! cost = 2                     # ascending = cheaper first (default 0)
//! triggers = ["**/*.rs", "Cargo.toml"]   # empty/missing = always runs
//! cache = true                 # false for gates that read commit metadata
//! ```
//!
//! Selection policy is deliberately over-selecting: a gate with no
//! triggers matches every diff, and an unparseable glob fails config
//! validation rather than silently never matching.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::git::GitRepo;
use crate::store::BrokerStore;
use crate::types::{GateFailureClass, GateStatus, NewGateResult};

pub const GATES_CONFIG_RELPATH: &str = ".aethyme/gates.toml";

#[derive(Debug, thiserror::Error)]
pub enum GateConfigError {
    #[error("no gates config at {0} (create it to define gates)")]
    Missing(PathBuf),
    #[error("gates.toml: {0}")]
    Parse(String),
    #[error("gate {gate:?}: invalid trigger glob {glob:?}: {message}")]
    BadGlob {
        gate: String,
        glob: String,
        message: String,
    },
}

/// One configured gate, with its compiled trigger set.
#[derive(Debug)]
pub struct Gate {
    pub name: String,
    pub command: String,
    pub cost: i64,
    pub triggers: Vec<String>,
    pub cache: bool,
    matcher: Option<GlobSet>,
}

impl Gate {
    /// Whether this gate is triggered by `path`. No triggers = always.
    pub fn matches(&self, path: &str) -> bool {
        match &self.matcher {
            None => true,
            Some(set) => set.is_match(path),
        }
    }
}

/// Load and validate `.aethyme/gates.toml`, sorted cheap-first.
pub fn load_gates(main_root: &Path) -> Result<Vec<Gate>, GateConfigError> {
    let path = main_root.join(GATES_CONFIG_RELPATH);
    let text =
        std::fs::read_to_string(&path).map_err(|_| GateConfigError::Missing(path.clone()))?;
    let value: toml::Value = text
        .parse()
        .map_err(|err: toml::de::Error| GateConfigError::Parse(err.to_string()))?;
    let entries = value
        .get("gate")
        .and_then(|gates| gates.as_array())
        .ok_or_else(|| GateConfigError::Parse("expected at least one [[gate]] table".into()))?;

    let mut gates = Vec::new();
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GateConfigError::Parse("gate missing string field 'name'".into()))?
            .to_string();
        let command = entry
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GateConfigError::Parse(format!("gate {name:?} missing string field 'command'"))
            })?
            .to_string();
        let cost = entry.get("cost").and_then(|v| v.as_integer()).unwrap_or(0);
        let cache = entry.get("cache").and_then(|v| v.as_bool()).unwrap_or(true);
        let triggers: Vec<String> = entry
            .get("triggers")
            .and_then(|v| v.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let matcher = if triggers.is_empty() {
            None
        } else {
            let mut builder = GlobSetBuilder::new();
            for glob in &triggers {
                builder.add(Glob::new(glob).map_err(|err| GateConfigError::BadGlob {
                    gate: name.clone(),
                    glob: glob.clone(),
                    message: err.to_string(),
                })?);
            }
            Some(builder.build().map_err(|err| GateConfigError::BadGlob {
                gate: name.clone(),
                glob: "<set>".into(),
                message: err.to_string(),
            })?)
        };
        gates.push(Gate {
            name,
            command,
            cost,
            triggers,
            cache,
            matcher,
        });
    }
    gates.sort_by(|a, b| a.cost.cmp(&b.cost).then(a.name.cmp(&b.name)));
    Ok(gates)
}

/// Why a gate was selected: the first changed file that triggered it
/// (`None` for always-run gates). Powers `--why`.
#[derive(Debug, serde::Serialize)]
pub struct Selection<'g> {
    #[serde(serialize_with = "gate_name")]
    pub gate: &'g Gate,
    pub triggered_by: Option<String>,
    #[serde(skip)]
    owner_paths: Vec<String>,
}

fn gate_name<S: serde::Serializer>(gate: &&Gate, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&gate.name)
}

/// Deterministic affected-gate selection for a set of changed files.
pub fn select_gates<'g>(gates: &'g [Gate], changed: &[String]) -> Vec<Selection<'g>> {
    let mut selections = Vec::new();
    for gate in gates {
        if gate.matcher.is_none() {
            selections.push(Selection {
                gate,
                triggered_by: None,
                owner_paths: Vec::new(),
            });
            continue;
        }
        let owner_paths = changed
            .iter()
            .filter(|path| gate.matches(path))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(hit) = owner_paths.first() {
            selections.push(Selection {
                gate,
                triggered_by: Some(hit.clone()),
                owner_paths,
            });
        }
    }
    selections
}

/// Outcome of running (or cache-resolving) one gate.
#[derive(Debug, serde::Serialize)]
pub struct GateRunOutcome {
    pub gate: String,
    /// Full Git tree object id proven by this result.
    pub tree_hash: String,
    pub status: GateStatus,
    pub failure_class: Option<GateFailureClass>,
    pub cached: bool,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub log_path: Option<String>,
}

/// Sink for human-readable gate progress. Production uses stderr; tests can
/// inject a collector without changing child stdout/stderr capture.
pub trait GateProgressSink: Send + Sync {
    fn report(&self, line: &str);
}

struct StderrGateProgressSink;

impl GateProgressSink for StderrGateProgressSink {
    fn report(&self, line: &str) {
        eprintln!("{line}");
    }
}

fn heartbeat_interval() -> Duration {
    let seconds = std::env::var("AETHYME_GATE_HEARTBEAT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(30);
    Duration::from_secs(seconds)
}

/// Directory holding pidfiles for in-flight gate runs, enabling
/// cross-process cancellation without a daemon. One file per running
/// gate: `<session>-<gate>.pid` containing `<pgid> <tree_hash>`.
fn running_dir(main_root: &Path) -> PathBuf {
    main_root.join(".aethyme/run/gates")
}

/// Kill in-flight gate runs for `session_id` whose tree differs from
/// `current_tree` (issue #18): they test a superseded state. Records a
/// `cancelled` result for each. Returns the cancelled gate names.
pub fn cancel_obsolete_runs(
    store: &mut BrokerStore,
    main_root: &Path,
    session_id: i64,
    current_tree: &str,
) -> Vec<String> {
    let dir = running_dir(main_root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let prefix = format!("{session_id}-");
    let mut cancelled = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = file_name.strip_suffix(".pid") else {
            continue;
        };
        let Some(gate_name) = stem.strip_prefix(&prefix) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let mut parts = content.split_whitespace();
        let (Some(pgid), Some(tree)) = (parts.next(), parts.next()) else {
            continue;
        };
        if tree == current_tree {
            continue;
        }
        // Kill the whole process group (the runner spawns each gate in
        // its own group for exactly this purpose). killpg directly:
        // the external `kill` utility on Linux parses "-<pgid>" as an
        // option and silently does nothing.
        if let Ok(pgid) = pgid.parse::<i32>() {
            unsafe {
                libc::killpg(pgid, libc::SIGTERM);
            }
        }
        let _ = std::fs::remove_file(entry.path());
        let _ = store.record_gate_result(&NewGateResult {
            gate_name: gate_name.to_string(),
            tree_hash: tree.to_string(),
            status: GateStatus::Cancelled,
            failure_class: None,
            exit_code: None,
            duration_ms: None,
            log_path: None,
            session_id: Some(session_id),
        });
        cancelled.push(gate_name.to_string());
    }
    cancelled
}

/// Run the affected gates for a checkout, cheap-first, with tree-hash
/// caching. `session_id` scopes cancellation and result attribution.
/// Stops after the first failure (later gates are pointless on a broken
/// tree — and cheaper gates ran first by construction).
pub fn run_affected(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    changed: &[String],
    session_id: Option<i64>,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    let progress = StderrGateProgressSink;
    run_affected_with_progress(
        store, main_root, checkout, gates, changed, session_id, &progress,
    )
}

/// Like [`run_affected`], with an injectable progress sink for tests and
/// future non-CLI surfaces.
pub fn run_affected_with_progress(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    changed: &[String],
    session_id: Option<i64>,
    progress: &dyn GateProgressSink,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    let selections = select_gates(gates, changed);
    run_selections(store, main_root, checkout, selections, session_id, progress)
}

/// Run EVERY configured gate cheap-first — no diff selection. This is
/// the full-tree "verified" definition shared by CI (`gates run --all`)
/// and the broker: the exact same executor as [`run_affected`], so
/// streaming progress, the tree-hash result cache, and fail-fast
/// semantics are identical by construction.
pub fn run_all(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    session_id: Option<i64>,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    let progress = StderrGateProgressSink;
    run_all_with_progress(store, main_root, checkout, gates, session_id, &progress)
}

/// Like [`run_all`], with an injectable progress sink.
pub fn run_all_with_progress(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    session_id: Option<i64>,
    progress: &dyn GateProgressSink,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    // Every gate, already cost-sorted by load_gates; `triggered_by` is
    // None because nothing was selected by a diff.
    let selections = gates
        .iter()
        .map(|gate| Selection {
            gate,
            triggered_by: None,
            owner_paths: Vec::new(),
        })
        .collect();
    run_selections(store, main_root, checkout, selections, session_id, progress)
}

struct GateOwnerLocks {
    _files: Vec<std::fs::File>,
}

impl GateOwnerLocks {
    fn acquire(
        owner_dir: &Path,
        gate_name: &str,
        owner_paths: &[String],
        progress: &dyn GateProgressSink,
    ) -> Result<Self, std::io::Error> {
        use std::os::fd::AsRawFd;

        std::fs::create_dir_all(owner_dir)?;
        let mut paths = gate_owner_lock_paths(owner_dir, gate_name, owner_paths);
        paths.sort();
        paths.dedup();
        if !paths.is_empty() {
            progress.report(&format!(
                "gate {gate_name} waiting for {} owner lock(s)",
                paths.len()
            ));
        }

        let mut files = Vec::with_capacity(paths.len());
        for path in paths {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)?;
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(std::io::Error::last_os_error());
            }
            files.push(file);
        }
        Ok(Self { _files: files })
    }
}

fn gate_owner_lock_paths(
    owner_dir: &Path,
    gate_name: &str,
    owner_paths: &[String],
) -> Vec<PathBuf> {
    gate_owner_scope(owner_paths)
        .into_iter()
        .map(|scope| {
            let name = format!(
                "{}-{}-{:016x}.lock",
                lock_segment(gate_name),
                lock_segment(&scope),
                stable_hash(gate_name, &scope)
            );
            owner_dir.join(name)
        })
        .collect()
}

fn gate_owner_scope(owner_paths: &[String]) -> Vec<String> {
    if owner_paths.is_empty() {
        return vec!["all".into()];
    }
    let mut scope = owner_paths.to_vec();
    scope.sort();
    scope.dedup();
    scope
}

fn lock_segment(value: &str) -> String {
    let mut segment = String::new();
    let mut last_was_dash = false;
    for ch in value.chars() {
        let next = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if !last_was_dash {
            Some('-')
        } else {
            None
        };
        if let Some(ch) = next {
            segment.push(ch);
            last_was_dash = ch == '-';
        }
        if segment.len() >= 48 {
            break;
        }
    }
    let trimmed = segment.trim_matches('-');
    if trimmed.is_empty() {
        "scope".into()
    } else {
        trimmed.into()
    }
}

fn stable_hash(gate_name: &str, scope: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in gate_name
        .as_bytes()
        .iter()
        .copied()
        .chain([0])
        .chain(scope.as_bytes().iter().copied())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn gate_worker_id(session_id: Option<i64>, gate_name: &str) -> String {
    let owner = match session_id {
        Some(session_id) => format!("s{session_id}"),
        None => format!("p{}", std::process::id()),
    };
    format!("{}-{}", owner, lock_segment(gate_name))
}

/// Shared executor for a pre-computed selection: cheap-first order (the
/// selection preserves `load_gates` sorting), tree-hash caching, one
/// process group per gate, fail-fast.
fn run_selections(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    selections: Vec<Selection<'_>>,
    session_id: Option<i64>,
    progress: &dyn GateProgressSink,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    let tree = checkout.working_tree_hash()?;
    if let Some(session_id) = session_id {
        cancel_obsolete_runs(store, main_root, session_id, &tree);
    }

    let log_dir = main_root.join(".aethyme/logs/gates");
    let _ = std::fs::create_dir_all(&log_dir);
    let run_dir = running_dir(main_root);
    let _ = std::fs::create_dir_all(&run_dir);

    let mut outcomes = Vec::new();
    for selection in selections {
        let gate = selection.gate;
        let worker_id = gate_worker_id(session_id, &gate.name);
        // Cache: conclusive result for this exact tree, any session. The
        // hit is recorded as an event so saved execution time is
        // measurable (kill-criterion accounting). Gates that inspect
        // commit metadata must opt out: the tree can stay identical while
        // commit bodies change.
        if gate.cache
            && let Some(hit) = store.cached_gate_result(&gate.name, &tree)?
        {
            let saved_ms = hit.duration_ms.unwrap_or(0);
            progress.report(&format!(
                "gate {} cached ({}, tree {}, saved {}ms)",
                gate.name,
                hit.status.as_str(),
                short_tree_hash(&tree),
                saved_ms
            ));
            let _ = store.append_event(
                crate::events::GATE_CACHED,
                session_id,
                Some(&crate::events::gate_cached_payload(
                    &gate.name,
                    &tree,
                    saved_ms,
                    hit.status,
                    cached_failure_class(hit.status),
                )),
            );
            let failed = hit.status == GateStatus::Fail;
            outcomes.push(GateRunOutcome {
                gate: gate.name.clone(),
                tree_hash: tree.clone(),
                status: hit.status,
                failure_class: cached_failure_class(hit.status),
                cached: true,
                exit_code: hit.exit_code,
                duration_ms: hit.duration_ms,
                log_path: hit.log_path,
            });
            if failed {
                break;
            }
            continue;
        }

        let owner_dir = run_dir.join("owners");
        let owner_locks =
            GateOwnerLocks::acquire(&owner_dir, &gate.name, &selection.owner_paths, progress)
                .map_err(|source| crate::BrokerError::Io {
                    path: owner_dir,
                    source,
                })?;
        let log_path = log_dir.join(format!(
            "{}-{}-{}.log",
            gate.name,
            &tree[..8.min(tree.len())],
            worker_id
        ));
        progress.report(&format!(
            "gate {} started (cost {}, tree {})",
            gate.name,
            gate.cost,
            short_tree_hash(&tree)
        ));
        let started = Instant::now();
        let status = run_gate_command(
            &gate.command,
            GateCommandContext {
                cwd: checkout.root(),
                log_path: &log_path,
                run_dir: &run_dir,
                session_id,
                gate_name: &gate.name,
                tree: &tree,
                worker_id: &worker_id,
                owner_paths: &selection.owner_paths,
                started,
                progress,
            },
        );
        drop(owner_locks);
        let duration_ms = started.elapsed().as_millis() as i64;
        let (gate_status, failure_class, exit_code) =
            classify_gate_result(&gate.command, &log_path, status);
        progress.report(&format!(
            "gate {} {} in {}s (tree {})",
            gate.name,
            gate_status.as_str(),
            started.elapsed().as_secs(),
            short_tree_hash(&tree)
        ));
        store.record_gate_result(&NewGateResult {
            gate_name: gate.name.clone(),
            tree_hash: tree.clone(),
            status: gate_status,
            failure_class,
            exit_code,
            duration_ms: Some(duration_ms),
            log_path: Some(log_path.to_string_lossy().into_owned()),
            session_id,
        })?;
        let failed = gate_status != GateStatus::Pass;
        outcomes.push(GateRunOutcome {
            gate: gate.name.clone(),
            tree_hash: tree.clone(),
            status: gate_status,
            failure_class,
            cached: false,
            exit_code,
            duration_ms: Some(duration_ms),
            log_path: Some(log_path.to_string_lossy().into_owned()),
        });
        if failed {
            break;
        }
    }
    Ok(outcomes)
}

fn short_tree_hash(tree_hash: &str) -> &str {
    &tree_hash[..12.min(tree_hash.len())]
}

fn classify_gate_result(
    command: &str,
    log_path: &Path,
    status: Result<Option<i32>, std::io::Error>,
) -> (GateStatus, Option<GateFailureClass>, Option<i64>) {
    match status {
        Ok(Some(0)) => (GateStatus::Pass, None, Some(0)),
        Ok(Some(code)) if is_timeout_error(code, log_path) => (
            GateStatus::Error,
            Some(GateFailureClass::Timeout),
            Some(code as i64),
        ),
        Ok(Some(code)) if is_resource_contention_error(command, log_path) => (
            GateStatus::Error,
            Some(GateFailureClass::ResourceContention),
            Some(code as i64),
        ),
        Ok(Some(code)) if is_environment_error(code, log_path) => (
            GateStatus::Error,
            Some(GateFailureClass::Environment),
            Some(code as i64),
        ),
        Ok(Some(code)) => (
            GateStatus::Fail,
            Some(GateFailureClass::TestFailure),
            Some(code as i64),
        ),
        // Killed by a signal (cancellation, OOM, operator kill): not a
        // verdict on the code. Recording a conclusive fail here poisons
        // the tree-hash cache — if the same tree recurs, the cached
        // "fail" rejects a submission without ever running the gate.
        Ok(None) => (GateStatus::Cancelled, None, None),
        Err(_) => (GateStatus::Error, Some(GateFailureClass::Environment), None),
    }
}

fn cached_failure_class(status: GateStatus) -> Option<GateFailureClass> {
    match status {
        GateStatus::Fail => Some(GateFailureClass::CachedPriorFail),
        _ => None,
    }
}

fn is_timeout_error(exit_code: i32, log_path: &Path) -> bool {
    if exit_code == 124 {
        return true;
    }
    let Ok(log) = std::fs::read_to_string(log_path) else {
        return false;
    };
    let lower = log.to_ascii_lowercase();
    lower.contains("timed out")
        || lower.contains("timeout exceeded")
        || lower.contains("command timed out")
}

fn is_resource_contention_error(command: &str, log_path: &Path) -> bool {
    if !command_mentions_cargo(command) {
        return log_contains_any(
            log_path,
            &[
                "database is locked",
                "resource busy",
                "resource temporarily unavailable",
                "text file busy",
                "too many open files",
                "no space left on device",
            ],
        );
    }
    let Ok(log) = std::fs::read_to_string(log_path) else {
        return false;
    };
    let lower = log.to_ascii_lowercase();
    let target_context = lower.contains(".fingerprint")
        || lower.contains("target/debug")
        || lower.contains("target/release")
        || lower.contains(".rlib")
        || lower.contains("build directory");
    if !target_context {
        return false;
    }

    const INFRA_PATTERNS: &[&str] = &[
        "extern location",
        "no such file or directory",
        "failed to lock",
        "failed to acquire",
        "failed to rename",
        "failed to remove",
        "failed to write",
        "file exists",
        "resource busy",
        "text file busy",
    ];
    INFRA_PATTERNS.iter().any(|pattern| lower.contains(pattern))
}

fn is_environment_error(exit_code: i32, log_path: &Path) -> bool {
    if exit_code == 126 || exit_code == 127 {
        return true;
    }
    log_contains_any(
        log_path,
        &[
            ": command not found",
            "command not found",
            "not found on path",
            "executable file not found",
            "permission denied",
            "cannot execute",
        ],
    )
}

fn log_contains_any(log_path: &Path, patterns: &[&str]) -> bool {
    let Ok(log) = std::fs::read_to_string(log_path) else {
        return false;
    };
    let lower = log.to_ascii_lowercase();
    patterns.iter().any(|pattern| lower.contains(pattern))
}

fn command_mentions_cargo(command: &str) -> bool {
    command
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-')
        .any(|part| part == "cargo")
}

/// Spawn one gate command (`sh -c`, own process group, output to log),
/// maintaining the pidfile for cancellation. Returns the exit code.
struct GateCommandContext<'a> {
    cwd: &'a Path,
    log_path: &'a Path,
    run_dir: &'a Path,
    session_id: Option<i64>,
    gate_name: &'a str,
    tree: &'a str,
    worker_id: &'a str,
    owner_paths: &'a [String],
    started: Instant,
    progress: &'a dyn GateProgressSink,
}

fn run_gate_command(
    command: &str,
    context: GateCommandContext<'_>,
) -> Result<Option<i32>, std::io::Error> {
    use std::os::unix::process::CommandExt;

    let log = std::fs::File::create(context.log_path)?;
    let log_err = log.try_clone()?;
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(context.cwd)
        .env("AETHYME_GATE_WORKER_ID", context.worker_id)
        .env("AETHYME_TEST_DB_SUFFIX", context.worker_id)
        .env("AETHYME_GATE_OWNER_PATHS", context.owner_paths.join(":"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .process_group(0)
        .spawn()?;

    let pidfile = context.session_id.map(|sid| {
        context
            .run_dir
            .join(format!("{sid}-{}.pid", context.gate_name))
    });
    if let Some(pidfile) = &pidfile {
        let _ = std::fs::write(pidfile, format!("{} {}", child.id(), context.tree));
    }
    let status = std::thread::scope(|scope| {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let interval = heartbeat_interval();
        scope.spawn(move || {
            loop {
                match done_rx.recv_timeout(interval) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        context.progress.report(&format!(
                            "gate {} running... {}s",
                            context.gate_name,
                            context.started.elapsed().as_secs()
                        ));
                    }
                }
            }
        });
        let status = child.wait();
        let _ = done_tx.send(());
        status
    });
    if let Some(pidfile) = &pidfile {
        let _ = std::fs::remove_file(pidfile);
    }
    // Killed-by-signal surfaces as no exit code; `None` lets the caller
    // record a non-conclusive `cancelled` row instead of a fake exit code.
    Ok(status?.code())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir.join(".aethyme")).unwrap();
        std::fs::write(dir.join(GATES_CONFIG_RELPATH), body).unwrap();
    }

    #[test]
    fn config_parses_sorts_cheap_first_and_rejects_bad_globs() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(
            tmp.path(),
            r#"
[[gate]]
name = "pytest"
command = "pytest -q"
cost = 2
triggers = ["**/*.py"]

[[gate]]
name = "lint"
command = "ruff check ."
cost = 1
triggers = ["**/*.py"]

[[gate]]
name = "always"
command = "true"
"#,
        );
        let gates = load_gates(tmp.path()).unwrap();
        assert_eq!(
            gates.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            vec!["always", "lint", "pytest"]
        );

        write_config(
            tmp.path(),
            "[[gate]]\nname = \"bad\"\ncommand = \"x\"\ntriggers = [\"[\"]\n",
        );
        assert!(matches!(
            load_gates(tmp.path()),
            Err(GateConfigError::BadGlob { .. })
        ));
    }

    #[test]
    fn selection_matrix_docs_only_diff_runs_no_test_gates() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(
            tmp.path(),
            r#"
[[gate]]
name = "pytest"
command = "pytest -q"
triggers = ["**/*.py", "pyproject.toml"]

[[gate]]
name = "cargo"
command = "cargo test"
triggers = ["**/*.rs", "**/Cargo.toml"]
"#,
        );
        let gates = load_gates(tmp.path()).unwrap();

        // Docs-only diff → zero gates.
        assert!(select_gates(&gates, &["docs/guide.md".into()]).is_empty());

        // Python diff → pytest only, and --why knows which file.
        let selections = select_gates(
            &gates,
            &[
                "src/auth.py".into(),
                "tests/test_auth.py".into(),
                "README.md".into(),
            ],
        );
        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].gate.name, "pytest");
        assert_eq!(selections[0].triggered_by.as_deref(), Some("src/auth.py"));
        assert_eq!(
            selections[0].owner_paths,
            vec!["src/auth.py".to_string(), "tests/test_auth.py".to_string()]
        );

        // Nested Cargo.toml matches the rooted-glob form.
        let selections = select_gates(&gates, &["crates/x/Cargo.toml".into()]);
        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].gate.name, "cargo");
    }
}
