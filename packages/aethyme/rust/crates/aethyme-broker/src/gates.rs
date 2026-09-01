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
//! resource_ttl_seconds = 300
//!
//! [[gate.resources]]
//! key = "database_port"
//! kind = "tcp_port"
//! start = 55000
//! end = 55999
//! ```
//!
//! Selection policy is deliberately over-selecting: a gate with no
//! triggers matches every diff, and an unparseable glob fails config
//! validation rather than silently never matching.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use globset::{Glob, GlobSet, GlobSetBuilder};
use sha2::{Digest, Sha256};

use crate::git::GitRepo;
use crate::store::BrokerStore;
use crate::types::{GateFailureClass, GateStatus, NewGateResult};

pub const GATES_CONFIG_RELPATH: &str = ".aethyme/gates.toml";

/// Whether a gate run may reuse a conclusive result for the same tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CachePolicy {
    #[default]
    Use,
    Bypass,
}

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
    #[error("gate {gate:?}: invalid host resource profile: {message}")]
    BadResources { gate: String, message: String },
}

/// One configured gate, with its compiled trigger set.
#[derive(Debug)]
pub struct Gate {
    pub name: String,
    pub command: String,
    pub cost: i64,
    pub triggers: Vec<String>,
    pub cache: bool,
    pub resources: Vec<crate::HostResourceRequirement>,
    pub resource_ttl_seconds: u64,
    /// Maximum time to wait for a contended host resource bundle. Zero
    /// preserves the historical fail-fast behavior.
    pub resource_wait_seconds: u64,
    pub managed_cache: Option<ManagedGateCache>,
    pub definition_hash: String,
    matcher: Option<GlobSet>,
}

/// A broker-owned, repository-scoped artifact cache used by one gate.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedGateCache {
    /// Stable logical name. It is never interpreted as a filesystem path.
    pub key: String,
    /// Rotate the cache before a run when its stored bytes exceed this bound.
    pub max_bytes: u64,
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
        let resources: Vec<crate::HostResourceRequirement> = entry
            .get("resources")
            .cloned()
            .map(toml::Value::try_into)
            .transpose()
            .map_err(|error| GateConfigError::BadResources {
                gate: name.clone(),
                message: error.to_string(),
            })?
            .unwrap_or_default();
        let resource_ttl_seconds = entry
            .get("resource_ttl_seconds")
            .and_then(toml::Value::as_integer)
            .map(|value| {
                u64::try_from(value).map_err(|_| GateConfigError::BadResources {
                    gate: name.clone(),
                    message: "resource_ttl_seconds must be positive".into(),
                })
            })
            .transpose()?
            .unwrap_or(300);
        let resource_wait_seconds = entry
            .get("resource_wait_seconds")
            .and_then(toml::Value::as_integer)
            .map(|value| {
                u64::try_from(value).map_err(|_| GateConfigError::BadResources {
                    gate: name.clone(),
                    message: "resource_wait_seconds must be non-negative".into(),
                })
            })
            .transpose()?
            .unwrap_or(0);
        let managed_cache: Option<ManagedGateCache> = entry
            .get("managed_cache")
            .cloned()
            .map(toml::Value::try_into)
            .transpose()
            .map_err(|error| GateConfigError::BadResources {
                gate: name.clone(),
                message: format!("invalid managed_cache: {error}"),
            })?;
        if let Some(cache) = &managed_cache {
            validate_managed_cache(cache).map_err(|message| GateConfigError::BadResources {
                gate: name.clone(),
                message,
            })?;
            if resources
                .iter()
                .any(|resource| resource.key == "managed_cache")
            {
                return Err(GateConfigError::BadResources {
                    gate: name.clone(),
                    message: "resource key 'managed_cache' is reserved by managed_cache".into(),
                });
            }
        }
        crate::validate_host_resource_requirements(&resources, resource_ttl_seconds).map_err(
            |error| GateConfigError::BadResources {
                gate: name.clone(),
                message: error.to_string(),
            },
        )?;
        let definition_hash = gate_definition_hash(
            &name,
            &command,
            cost,
            &triggers,
            cache,
            &resources,
            resource_ttl_seconds,
            resource_wait_seconds,
            managed_cache.as_ref(),
        );

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
            resources,
            resource_ttl_seconds,
            resource_wait_seconds,
            managed_cache,
            definition_hash,
            matcher,
        });
    }
    gates.sort_by(|a, b| a.cost.cmp(&b.cost).then(a.name.cmp(&b.name)));
    Ok(gates)
}

fn gate_definition_hash(
    name: &str,
    command: &str,
    cost: i64,
    triggers: &[String],
    cache: bool,
    resources: &[crate::HostResourceRequirement],
    resource_ttl_seconds: u64,
    resource_wait_seconds: u64,
    managed_cache: Option<&ManagedGateCache>,
) -> String {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "name": name,
        "command": command,
        "cost": cost,
        "triggers": triggers,
        "cache": cache,
        "resources": resources,
        "resource_ttl_seconds": resource_ttl_seconds,
        "resource_wait_seconds": resource_wait_seconds,
        "managed_cache": managed_cache,
    }))
    .expect("gate definition contains only serializable values");
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_managed_cache(cache: &ManagedGateCache) -> Result<(), String> {
    if cache.key.is_empty()
        || cache.key.len() > 64
        || !cache
            .key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || cache.key == "."
        || cache.key == ".."
    {
        return Err("managed_cache.key must be 1-64 ASCII letters, digits, '.', '_' or '-'".into());
    }
    if cache.max_bytes == 0 {
        return Err("managed_cache.max_bytes must be positive".into());
    }
    Ok(())
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
    /// Digest of the command, triggers, cache policy, and resource profile.
    pub definition_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_lease: Option<GateResourceProvenance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_cache: Option<ManagedGateCacheProvenance>,
    pub status: GateStatus,
    pub failure_class: Option<GateFailureClass>,
    pub cached: bool,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    /// Time spent waiting for owner locks, host resources, and cache prep.
    pub wait_duration_ms: Option<i64>,
    /// Time from command spawn until the first stdout/stderr byte appeared.
    pub first_output_ms: Option<i64>,
    /// Combined stdout/stderr bytes captured without exposing their content.
    pub output_bytes: Option<i64>,
    pub log_path: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ManagedGateCacheProvenance {
    pub key: String,
    pub max_bytes: u64,
    pub bytes_before: u64,
    pub bytes_after: Option<u64>,
    pub rotated_before_run: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GateResourceProvenance {
    pub lease_id: String,
    pub generation: u64,
    pub expires_at: i64,
    pub allocations: Vec<crate::HostResourceAllocation>,
}

/// One ref update received from Git's `pre-push` hook protocol.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct PrePushUpdate {
    pub local_ref: String,
    pub local_sha: String,
    pub remote_ref: String,
    pub remote_sha: String,
}

/// Reviewed, read-only interpretation of a `pre-push` hook invocation.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct PrePushPlan {
    pub remote: String,
    pub pushed_sha: Option<String>,
    pub updates: Vec<PrePushUpdate>,
}

/// Result of the opt-in repository-owned pre-push adapter.
#[derive(Debug, serde::Serialize)]
pub struct PrePushReport {
    pub plan: PrePushPlan,
    pub gate_outcomes: Vec<GateRunOutcome>,
}

#[derive(Debug, thiserror::Error)]
pub enum PrePushValidationError {
    #[error(transparent)]
    Git(#[from] crate::GitError),
    #[error(
        "pre-push received no ref updates on stdin; invoke this command from a Git pre-push hook"
    )]
    NoUpdates,
    #[error(
        "invalid pre-push update on line {line}; expected <local-ref> <local-sha> <remote-ref> <remote-sha>"
    )]
    MalformedUpdate { line: usize },
    #[error("pre-push cannot prove multiple different local tips in one checkout: {shas}")]
    MultipleTips { shas: String },
    #[error(
        "pre-push local tip {pushed_sha} is not this checkout's HEAD {head_sha}; run validation from a clean worktree checked out at the pushed tip"
    )]
    TipNotHead {
        pushed_sha: String,
        head_sha: String,
    },
    #[error(
        "pre-push requires a clean checkout so evidence matches the pushed commit; dirty paths: {paths}"
    )]
    DirtyCheckout { paths: String },
}

/// Parse Git's pre-push stdin and prove that a single clean checkout can
/// truthfully validate every non-deletion update. Deletion-only pushes need no
/// content validation and therefore produce a plan without `pushed_sha`.
pub fn plan_pre_push(
    checkout: &GitRepo,
    remote: &str,
    input: &str,
) -> Result<PrePushPlan, PrePushValidationError> {
    let mut updates = Vec::new();
    for (index, line) in input.lines().enumerate() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 4 {
            return Err(PrePushValidationError::MalformedUpdate { line: index + 1 });
        }
        updates.push(PrePushUpdate {
            local_ref: fields[0].to_string(),
            local_sha: fields[1].to_string(),
            remote_ref: fields[2].to_string(),
            remote_sha: fields[3].to_string(),
        });
    }
    if updates.is_empty() {
        return Err(PrePushValidationError::NoUpdates);
    }
    updates.sort_by(|left, right| {
        left.local_ref
            .cmp(&right.local_ref)
            .then(left.remote_ref.cmp(&right.remote_ref))
            .then(left.local_sha.cmp(&right.local_sha))
    });

    let pushed_shas: BTreeSet<_> = updates
        .iter()
        .filter(|update| !update.local_sha.chars().all(|character| character == '0'))
        .map(|update| update.local_sha.clone())
        .collect();
    let pushed_sha = match pushed_shas.len() {
        0 => None,
        1 => pushed_shas.into_iter().next(),
        _ => {
            return Err(PrePushValidationError::MultipleTips {
                shas: pushed_shas.into_iter().collect::<Vec<_>>().join(", "),
            });
        }
    };
    if let Some(pushed_sha) = &pushed_sha {
        let head_sha = checkout.head_commit()?;
        if pushed_sha != &head_sha {
            return Err(PrePushValidationError::TipNotHead {
                pushed_sha: pushed_sha.clone(),
                head_sha,
            });
        }
        let dirty = checkout.dirty_paths()?;
        if !dirty.is_empty() {
            return Err(PrePushValidationError::DirtyCheckout {
                paths: dirty.into_iter().take(10).collect::<Vec<_>>().join(", "),
            });
        }
    }

    Ok(PrePushPlan {
        remote: remote.to_string(),
        pushed_sha,
        updates,
    })
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

pub(crate) struct GateExecutionContext<'a> {
    pub cache_policy: CachePolicy,
    pub progress: &'a dyn GateProgressSink,
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
            definition_hash: String::new(),
            status: GateStatus::Cancelled,
            failure_class: None,
            exit_code: None,
            duration_ms: None,
            wait_duration_ms: None,
            first_output_ms: None,
            output_bytes: None,
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
pub(crate) fn run_affected(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    changed: &[String],
    session_id: Option<i64>,
    cache_policy: CachePolicy,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    let progress = StderrGateProgressSink;
    run_affected_with_progress(
        store,
        main_root,
        checkout,
        gates,
        changed,
        session_id,
        GateExecutionContext {
            cache_policy,
            progress: &progress,
        },
    )
}

/// Like [`run_affected`], with an injectable progress sink.
pub(crate) fn run_affected_with_progress(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    changed: &[String],
    session_id: Option<i64>,
    context: GateExecutionContext<'_>,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    let selections = select_gates(gates, changed);
    run_selections(
        store,
        main_root,
        checkout,
        selections,
        session_id,
        context.cache_policy,
        context.progress,
    )
}

/// Run EVERY configured gate cheap-first — no diff selection. This is
/// the full-tree "verified" definition shared by CI (`gates run --all`)
/// and the broker: the exact same executor as [`run_affected`], so
/// streaming progress, the tree-hash result cache, and fail-fast
/// semantics are identical by construction.
pub(crate) fn run_all(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    session_id: Option<i64>,
    cache_policy: CachePolicy,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    let progress = StderrGateProgressSink;
    run_all_with_progress(
        store,
        main_root,
        checkout,
        gates,
        session_id,
        cache_policy,
        &progress,
    )
}

/// Like [`run_all`], with an injectable progress sink.
pub(crate) fn run_all_with_progress(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    session_id: Option<i64>,
    cache_policy: CachePolicy,
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
    run_selections(
        store,
        main_root,
        checkout,
        selections,
        session_id,
        cache_policy,
        progress,
    )
}

/// Run one explicitly selected gate. Unlike affected selection, an exact
/// name is authoritative even when its path triggers do not match the diff.
pub(crate) fn run_named(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    changed: &[String],
    name: &str,
    session_id: Option<i64>,
    cache_policy: CachePolicy,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    let gate = gates.iter().find(|gate| gate.name == name).ok_or_else(|| {
        crate::broker::BrokerOpError::UnknownGate {
            name: name.to_string(),
        }
    })?;
    let owner_paths = changed
        .iter()
        .filter(|path| gate.matches(path))
        .cloned()
        .collect();
    let progress = StderrGateProgressSink;
    run_selections(
        store,
        main_root,
        checkout,
        vec![Selection {
            gate,
            triggered_by: None,
            owner_paths,
        }],
        session_id,
        cache_policy,
        &progress,
    )
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

struct GateResourceRuntime {
    grant: crate::HostResourceGrant,
    ttl_seconds: u64,
}

struct ManagedGateCacheRuntime {
    directory: PathBuf,
    provenance: ManagedGateCacheProvenance,
}

impl GateResourceRuntime {
    fn release(&mut self) -> Result<(), crate::HostResourceError> {
        let mut coordinator = crate::HostResourceCoordinator::open_default()?;
        self.grant.lease = coordinator.release(
            &self.grant.lease.lease_id,
            self.grant.lease.generation,
            &self.grant.ownership_token,
        )?;
        Ok(())
    }
}

fn acquire_gate_resources(
    gate: &Gate,
    checkout: &GitRepo,
    tree: &str,
    worker_id: &str,
    progress: &dyn GateProgressSink,
) -> Result<Option<GateResourceRuntime>, String> {
    if gate.resources.is_empty() && gate.managed_cache.is_none() {
        return Ok(None);
    }
    let repository = git_origin_fingerprint(checkout);
    let worktree_fingerprint = sha256_text(&checkout.root().to_string_lossy());
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let request_id = sha256_text(&format!(
        "{repository}:{worktree_fingerprint}:{tree}:{}:{worker_id}:{nonce}",
        std::process::id()
    ));
    let mut resources = gate.resources.clone();
    if let Some(cache) = &gate.managed_cache {
        resources.push(crate::HostResourceRequirement {
            key: "managed_cache".into(),
            resource: crate::HostResourceKind::ExclusiveKey {
                name: format!("aethyme-gate-cache:{repository}:{}", cache.key),
            },
        });
    }
    let request = crate::HostResourceRequest {
        schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
        request_id,
        repository,
        worktree_fingerprint,
        run_id: format!("{}-{}", worker_id, short_tree_hash(tree)),
        ttl_seconds: gate.resource_ttl_seconds,
        holder_pid: Some(std::process::id()),
        resources,
    };
    let mut coordinator =
        crate::HostResourceCoordinator::open_default().map_err(|error| error.to_string())?;
    let mut next_report = std::time::Instant::now();
    let grant = coordinator
        .acquire_with_wait(
            &request,
            std::time::Duration::from_secs(gate.resource_wait_seconds),
            |message| {
                let now = std::time::Instant::now();
                if now >= next_report {
                    progress.report(&format!(
                        "gate {} waiting for host resources: {}",
                        gate.name, message
                    ));
                    next_report = now + std::time::Duration::from_secs(5);
                }
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(Some(GateResourceRuntime {
        grant,
        ttl_seconds: gate.resource_ttl_seconds,
    }))
}

fn git_origin_fingerprint(repo: &GitRepo) -> String {
    let material = repo
        .resolve_remote_target("origin", None)
        .map(|target| target.coordination_key)
        .unwrap_or_else(|_| {
            repo.root()
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("repository")
                .to_string()
        });
    sha256_text(&material)
}

fn prepare_managed_gate_cache(
    policy: Option<&ManagedGateCache>,
    repository: &str,
    progress: &dyn GateProgressSink,
    gate_name: &str,
) -> Result<Option<ManagedGateCacheRuntime>, std::io::Error> {
    if policy.is_none() {
        return Ok(None);
    }
    let root = crate::host_state::default_host_cache_dir().ok_or_else(|| {
        std::io::Error::other("cannot find per-user cache directory; set AETHYME_HOST_CACHE_DIR")
    })?;
    prepare_managed_gate_cache_in(policy, repository, progress, gate_name, &root)
}

fn prepare_managed_gate_cache_in(
    policy: Option<&ManagedGateCache>,
    repository: &str,
    progress: &dyn GateProgressSink,
    gate_name: &str,
    root: &Path,
) -> Result<Option<ManagedGateCacheRuntime>, std::io::Error> {
    let Some(policy) = policy else {
        return Ok(None);
    };
    std::fs::create_dir_all(&root)?;
    crate::host_state::protect_host_state_path(&root, true)?;
    let repository_root = root.join("gates").join(repository);
    std::fs::create_dir_all(&repository_root)?;
    let directory = repository_root.join(&policy.key);
    let bytes_before = directory_usage(&directory)?;
    let rotated_before_run = bytes_before > policy.max_bytes;
    if rotated_before_run {
        progress.report(&format!(
            "gate {gate_name} rotating managed cache {} ({} bytes exceeds {} bytes)",
            policy.key, bytes_before, policy.max_bytes
        ));
        let retired = repository_root.join(format!(
            ".{}.retired-{}-{}",
            policy.key,
            epoch_ms(),
            std::process::id()
        ));
        std::fs::rename(&directory, &retired)?;
        std::fs::create_dir_all(&directory)?;
        std::fs::remove_dir_all(retired)?;
    } else {
        std::fs::create_dir_all(&directory)?;
    }
    Ok(Some(ManagedGateCacheRuntime {
        directory,
        provenance: ManagedGateCacheProvenance {
            key: policy.key.clone(),
            max_bytes: policy.max_bytes,
            bytes_before,
            bytes_after: None,
            rotated_before_run,
        },
    }))
}

fn directory_usage(path: &Path) -> Result<u64, std::io::Error> {
    if !path.exists() {
        return Ok(0);
    }
    let mut bytes = 0_u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = entry.file_type()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                bytes = bytes.saturating_add(entry.metadata()?.len());
            }
        }
    }
    Ok(bytes)
}

fn sha256_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
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
    cache_policy: CachePolicy,
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
    let mut expensive_advisories_surfaced = false;
    for selection in selections {
        let gate = selection.gate;
        let worker_id = gate_worker_id(session_id, &gate.name);
        // Cache: conclusive result for this exact tree, any session. The
        // hit is recorded as an event so saved execution time is
        // measurable (kill-criterion accounting). Gates that inspect
        // commit metadata must opt out: the tree can stay identical while
        // commit bodies change.
        if cache_policy == CachePolicy::Use
            && gate.cache
            && let Some(hit) =
                store.cached_gate_result_for_definition(&gate.name, &tree, &gate.definition_hash)?
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
                definition_hash: gate.definition_hash.clone(),
                resource_lease: None,
                managed_cache: None,
                status: hit.status,
                failure_class: cached_failure_class(hit.status),
                cached: true,
                exit_code: hit.exit_code,
                duration_ms: hit.duration_ms,
                wait_duration_ms: Some(0),
                first_output_ms: hit.first_output_ms,
                output_bytes: hit.output_bytes,
                log_path: hit.log_path,
            });
            if failed {
                break;
            }
            continue;
        }

        if gate.cost > 1 && !expensive_advisories_surfaced {
            expensive_advisories_surfaced = true;
            if let Some(session_id) = session_id
                && let Ok(advisories) = store.outstanding_advisories_for_session(session_id)
            {
                let _ = store
                    .record_advisories_shown(&advisories, crate::AdvisoryDeliverySurface::PreGate);
                for line in crate::advisories::session_notice_lines(&advisories) {
                    progress.report(&line);
                }
            }
        }

        let wait_started = Instant::now();
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
        let mut resource_runtime =
            match acquire_gate_resources(gate, checkout, &tree, &worker_id, progress) {
                Ok(runtime) => runtime,
                Err(message) => {
                    let _ = std::fs::write(
                        &log_path,
                        format!("aethyme host resource acquisition failed: {message}\n"),
                    );
                    progress.report(&format!(
                        "gate {} blocked by host resources: {}",
                        gate.name, message
                    ));
                    drop(owner_locks);
                    store.record_gate_result(&NewGateResult {
                        gate_name: gate.name.clone(),
                        tree_hash: tree.clone(),
                        definition_hash: gate.definition_hash.clone(),
                        status: GateStatus::Error,
                        failure_class: Some(GateFailureClass::ResourceContention),
                        exit_code: None,
                        duration_ms: Some(0),
                        wait_duration_ms: Some(wait_started.elapsed().as_millis() as i64),
                        first_output_ms: None,
                        output_bytes: Some(0),
                        log_path: Some(log_path.to_string_lossy().into_owned()),
                        session_id,
                    })?;
                    outcomes.push(GateRunOutcome {
                        gate: gate.name.clone(),
                        tree_hash: tree.clone(),
                        definition_hash: gate.definition_hash.clone(),
                        resource_lease: None,
                        managed_cache: None,
                        status: GateStatus::Error,
                        failure_class: Some(GateFailureClass::ResourceContention),
                        cached: false,
                        exit_code: None,
                        duration_ms: Some(0),
                        wait_duration_ms: Some(wait_started.elapsed().as_millis() as i64),
                        first_output_ms: None,
                        output_bytes: Some(0),
                        log_path: Some(log_path.to_string_lossy().into_owned()),
                    });
                    break;
                }
            };
        let resource_provenance = resource_runtime
            .as_ref()
            .map(|runtime| GateResourceProvenance {
                lease_id: runtime.grant.lease.lease_id.clone(),
                generation: runtime.grant.lease.generation,
                expires_at: runtime.grant.lease.expires_at,
                allocations: runtime.grant.lease.allocations.clone(),
            });
        let repository = git_origin_fingerprint(checkout);
        let mut managed_cache_runtime = match prepare_managed_gate_cache(
            gate.managed_cache.as_ref(),
            &repository,
            progress,
            &gate.name,
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                let message = format!("managed cache preparation failed: {error}");
                let _ = std::fs::write(&log_path, format!("aethyme {message}\n"));
                progress.report(&format!("gate {} environment error: {message}", gate.name));
                let _ = resource_runtime.as_mut().map(GateResourceRuntime::release);
                drop(owner_locks);
                store.record_gate_result(&NewGateResult {
                    gate_name: gate.name.clone(),
                    tree_hash: tree.clone(),
                    definition_hash: gate.definition_hash.clone(),
                    status: GateStatus::Error,
                    failure_class: Some(GateFailureClass::Environment),
                    exit_code: None,
                    duration_ms: Some(0),
                    wait_duration_ms: Some(wait_started.elapsed().as_millis() as i64),
                    first_output_ms: None,
                    output_bytes: Some(0),
                    log_path: Some(log_path.to_string_lossy().into_owned()),
                    session_id,
                })?;
                outcomes.push(GateRunOutcome {
                    gate: gate.name.clone(),
                    tree_hash: tree.clone(),
                    definition_hash: gate.definition_hash.clone(),
                    resource_lease: resource_provenance,
                    managed_cache: None,
                    status: GateStatus::Error,
                    failure_class: Some(GateFailureClass::Environment),
                    cached: false,
                    exit_code: None,
                    duration_ms: Some(0),
                    wait_duration_ms: Some(wait_started.elapsed().as_millis() as i64),
                    first_output_ms: None,
                    output_bytes: Some(0),
                    log_path: Some(log_path.to_string_lossy().into_owned()),
                });
                break;
            }
        };
        let wait_duration_ms = wait_started.elapsed().as_millis() as i64;
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
                resources: resource_runtime.as_ref(),
                managed_cache: managed_cache_runtime.as_ref(),
            },
        );
        if let Some(cache) = managed_cache_runtime.as_mut() {
            cache.provenance.bytes_after = directory_usage(&cache.directory).ok();
        }
        let release_error = resource_runtime
            .as_mut()
            .and_then(|runtime| runtime.release().err())
            .map(|error| format!("host resource release failed: {error}"));
        drop(owner_locks);
        let duration_ms = started.elapsed().as_millis() as i64;
        let status = match (status, release_error) {
            (Ok(mut outcome), release_error) => {
                if outcome.resource_error.is_none() {
                    outcome.resource_error = release_error;
                }
                Ok(outcome)
            }
            // A failed spawn or wait is normally an environment error, but a
            // simultaneous release failure is the more urgent invariant: the
            // host bundle may still be owned and must be reconciled as such.
            (Err(_), Some(release_error)) => Ok(GateCommandOutcome {
                exit_code: None,
                resource_error: Some(release_error),
                first_output_ms: None,
                output_bytes: 0,
            }),
            (Err(error), None) => Err(error),
        };
        let first_output_ms = status
            .as_ref()
            .ok()
            .and_then(|outcome| outcome.first_output_ms);
        let output_bytes = status
            .as_ref()
            .ok()
            .map(|outcome| outcome.output_bytes as i64);
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
            definition_hash: gate.definition_hash.clone(),
            status: gate_status,
            failure_class,
            exit_code,
            duration_ms: Some(duration_ms),
            wait_duration_ms: Some(wait_duration_ms),
            first_output_ms,
            output_bytes,
            log_path: Some(log_path.to_string_lossy().into_owned()),
            session_id,
        })?;
        let failed = gate_status != GateStatus::Pass;
        outcomes.push(GateRunOutcome {
            gate: gate.name.clone(),
            tree_hash: tree.clone(),
            definition_hash: gate.definition_hash.clone(),
            resource_lease: resource_provenance,
            managed_cache: managed_cache_runtime.map(|runtime| runtime.provenance),
            status: gate_status,
            failure_class,
            cached: false,
            exit_code,
            duration_ms: Some(duration_ms),
            wait_duration_ms: Some(wait_duration_ms),
            first_output_ms,
            output_bytes,
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
    status: Result<GateCommandOutcome, std::io::Error>,
) -> (GateStatus, Option<GateFailureClass>, Option<i64>) {
    match status {
        Ok(GateCommandOutcome {
            exit_code,
            resource_error: Some(error),
            ..
        }) => {
            let _ = append_gate_log(log_path, &format!("aethyme host resource error: {error}\n"));
            (
                GateStatus::Error,
                Some(GateFailureClass::ResourceContention),
                exit_code.map(i64::from),
            )
        }
        Ok(GateCommandOutcome {
            exit_code: Some(0),
            resource_error: None,
            ..
        }) => (GateStatus::Pass, None, Some(0)),
        Ok(GateCommandOutcome {
            exit_code: Some(code),
            resource_error: None,
            ..
        }) if is_timeout_error(code, log_path) => (
            GateStatus::Error,
            Some(GateFailureClass::Timeout),
            Some(code as i64),
        ),
        Ok(GateCommandOutcome {
            exit_code: Some(code),
            resource_error: None,
            ..
        }) if is_resource_contention_error(command, log_path) => (
            GateStatus::Error,
            Some(GateFailureClass::ResourceContention),
            Some(code as i64),
        ),
        Ok(GateCommandOutcome {
            exit_code: Some(code),
            resource_error: None,
            ..
        }) if is_environment_error(code, log_path) => (
            GateStatus::Error,
            Some(GateFailureClass::Environment),
            Some(code as i64),
        ),
        Ok(GateCommandOutcome {
            exit_code: Some(code),
            resource_error: None,
            ..
        }) => (
            GateStatus::Fail,
            Some(GateFailureClass::TestFailure),
            Some(code as i64),
        ),
        // Killed by a signal (cancellation, OOM, operator kill): not a
        // verdict on the code. Recording a conclusive fail here poisons
        // the tree-hash cache — if the same tree recurs, the cached
        // "fail" rejects a submission without ever running the gate.
        Ok(GateCommandOutcome {
            exit_code: None,
            resource_error: None,
            ..
        }) => (GateStatus::Cancelled, None, None),
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
    resources: Option<&'a GateResourceRuntime>,
    managed_cache: Option<&'a ManagedGateCacheRuntime>,
}

struct GateCommandOutcome {
    exit_code: Option<i32>,
    resource_error: Option<String>,
    first_output_ms: Option<i64>,
    output_bytes: u64,
}

fn run_gate_command(
    command: &str,
    context: GateCommandContext<'_>,
) -> Result<GateCommandOutcome, std::io::Error> {
    use std::os::unix::process::CommandExt;

    let log = std::fs::File::create(context.log_path)?;
    let log_err = log.try_clone()?;
    let mut process = std::process::Command::new("sh");
    process
        .arg("-c")
        .arg(command)
        .current_dir(context.cwd)
        .env("AETHYME_GATE_WORKER_ID", context.worker_id)
        .env("AETHYME_TEST_DB_SUFFIX", context.worker_id)
        .env("AETHYME_GATE_OWNER_PATHS", context.owner_paths.join(":"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .process_group(0);
    if let Some(resources) = context.resources {
        for (key, value) in resources.grant.environment() {
            process.env(key, value);
        }
    }
    if let Some(cache) = context.managed_cache {
        process.env("AETHYME_GATE_CACHE_DIR", &cache.directory);
    }
    let mut child = process.spawn()?;

    let pidfile = context.session_id.map(|sid| {
        context
            .run_dir
            .join(format!("{sid}-{}.pid", context.gate_name))
    });
    if let Some(pidfile) = &pidfile {
        let _ = std::fs::write(pidfile, format!("{} {}", child.id(), context.tree));
    }
    let fatal_resource_error = std::sync::Arc::new(std::sync::Mutex::new(None));
    let first_output = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(-1));
    let output_monitor_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let status = std::thread::scope(|scope| {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let progress_interval = heartbeat_interval();
        let renewal = context
            .resources
            .map(|runtime| (runtime.grant.clone(), runtime.ttl_seconds));
        let interval = renewal
            .as_ref()
            .map(|(_, ttl)| Duration::from_secs((ttl / 3).max(1)))
            .map(|renewal| renewal.min(progress_interval))
            .unwrap_or(progress_interval);
        let process_group = child.id() as i32;
        let thread_error = fatal_resource_error.clone();
        let monitor_result = first_output.clone();
        let monitor_done = output_monitor_done.clone();
        scope.spawn(move || {
            while !monitor_done.load(std::sync::atomic::Ordering::Acquire) {
                if std::fs::metadata(context.log_path)
                    .map(|metadata| metadata.len() > 0)
                    .unwrap_or(false)
                {
                    let elapsed = context.started.elapsed().as_millis() as i64;
                    let _ = monitor_result.compare_exchange(
                        -1,
                        elapsed,
                        std::sync::atomic::Ordering::AcqRel,
                        std::sync::atomic::Ordering::Acquire,
                    );
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        });
        scope.spawn(move || {
            let mut renewal = renewal;
            let mut last_progress = Instant::now();
            loop {
                match done_rx.recv_timeout(interval) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if let Some((grant, ttl_seconds)) = renewal.as_mut() {
                            match crate::HostResourceCoordinator::open_default().and_then(
                                |mut coordinator| {
                                    coordinator.renew(
                                        &grant.lease.lease_id,
                                        grant.lease.generation,
                                        &grant.ownership_token,
                                        *ttl_seconds,
                                    )
                                },
                            ) {
                                Ok(lease) => grant.lease = lease,
                                Err(error) => {
                                    // Keep retrying while the last confirmed TTL still grants
                                    // authority. Stop the process before that authority expires.
                                    if epoch_ms().saturating_add(1_000) >= grant.lease.expires_at {
                                        if let Ok(mut slot) = thread_error.lock() {
                                            *slot = Some(error.to_string());
                                        }
                                        unsafe {
                                            libc::killpg(process_group, libc::SIGTERM);
                                        }
                                        break;
                                    }
                                }
                            }
                        }
                        if last_progress.elapsed() >= progress_interval {
                            context.progress.report(&format!(
                                "gate {} running... {}s",
                                context.gate_name,
                                context.started.elapsed().as_secs()
                            ));
                            last_progress = Instant::now();
                        }
                    }
                }
            }
        });
        let status = child.wait();
        output_monitor_done.store(true, std::sync::atomic::Ordering::Release);
        let _ = done_tx.send(());
        status
    });
    if let Some(pidfile) = &pidfile {
        let _ = std::fs::remove_file(pidfile);
    }
    let resource_error = fatal_resource_error
        .lock()
        .ok()
        .and_then(|mut error| error.take());
    // Killed-by-signal surfaces as no exit code; absent a resource error,
    // `None` remains a non-conclusive cancellation.
    let output_bytes = std::fs::metadata(context.log_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let observed_first_output = first_output.load(std::sync::atomic::Ordering::Acquire);
    Ok(GateCommandOutcome {
        exit_code: status?.code(),
        resource_error,
        first_output_ms: match observed_first_output {
            -1 if output_bytes > 0 => Some(context.started.elapsed().as_millis() as i64),
            -1 => None,
            elapsed => Some(elapsed),
        },
        output_bytes,
    })
}

fn append_gate_log(path: &Path, message: &str) -> Result<(), std::io::Error> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    file.write_all(message.as_bytes())
}

fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
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
resource_ttl_seconds = 60
resource_wait_seconds = 15

[gate.managed_cache]
key = "python-env"
max_bytes = 1048576

[[gate.resources]]
key = "database_port"
kind = "tcp_port"
start = 55000
end = 55999

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
        let pytest = gates.iter().find(|gate| gate.name == "pytest").unwrap();
        assert_eq!(pytest.resource_ttl_seconds, 60);
        assert_eq!(pytest.resource_wait_seconds, 15);
        assert_eq!(
            pytest.managed_cache,
            Some(ManagedGateCache {
                key: "python-env".into(),
                max_bytes: 1_048_576,
            })
        );
        assert_eq!(pytest.resources.len(), 1);
        assert!(matches!(
            pytest.resources[0].resource,
            crate::HostResourceKind::TcpPort {
                start: 55000,
                end: 55999
            }
        ));
        assert_eq!(pytest.definition_hash.len(), 64);

        write_config(
            tmp.path(),
            "[[gate]]\nname = \"bad\"\ncommand = \"x\"\ntriggers = [\"[\"]\n",
        );
        assert!(matches!(
            load_gates(tmp.path()),
            Err(GateConfigError::BadGlob { .. })
        ));

        write_config(
            tmp.path(),
            "[[gate]]\nname='bad-resource'\ncommand='x'\nresource_ttl_seconds=1\n\
             [[gate.resources]]\nkey='slot'\nkind='capacity'\npool='test'\nunits=1\nlimit=1\n",
        );
        assert!(matches!(
            load_gates(tmp.path()),
            Err(GateConfigError::BadResources { .. })
        ));
    }

    #[test]
    fn managed_cache_rotates_only_its_broker_owned_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = ManagedGateCache {
            key: "cargo".into(),
            max_bytes: 3,
        };
        let directory = tmp.path().join("gates/repository/cargo");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("large"), b"1234").unwrap();

        let runtime = prepare_managed_gate_cache_in(
            Some(&policy),
            "repository",
            &StderrGateProgressSink,
            "test",
            tmp.path(),
        )
        .unwrap()
        .unwrap();

        assert!(runtime.provenance.rotated_before_run);
        assert_eq!(runtime.provenance.bytes_before, 4);
        assert!(runtime.directory.is_dir());
        assert!(!runtime.directory.join("large").exists());
        assert_eq!(
            std::fs::read_dir(tmp.path().join("gates/repository"))
                .unwrap()
                .count(),
            1
        );
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
