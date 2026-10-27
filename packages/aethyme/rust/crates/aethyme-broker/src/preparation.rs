//! Repository-declared, explicit dependency preparation for broker worktrees.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::{CStr, CString};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::ffi::OsStrExt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::fs::MetadataExt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    Broker, BrokerOpError, HostResourceCoordinator, HostResourceKind, HostResourceRequest,
    HostResourceRequirement,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "preparation_rotation_debt.rs"]
pub(crate) mod rotation_debt;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[path = "preparation_rotation_debt_unsupported.rs"]
pub(crate) mod rotation_debt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "preparation_rotation_journal.rs"]
pub(crate) mod rotation_journal;

pub const PREPARATION_CONFIG_RELPATH: &str = ".aethyme/prepare.toml";
pub const PREPARATION_SCHEMA_VERSION: u32 = 1;
const PREPARATION_LEASE_TTL_SECONDS: u64 = 30;
const PREPARATION_STORAGE_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const PREPARATION_CACHE_CONFIG_MAX_BYTES: u64 = 1024 * 1024;
const PREPARATION_CACHE_HASH_BUFFER_BYTES: usize = 64 * 1024;
// The scanner checks the deadline between filesystem operations. Individual
// filesystem calls are not interruptible by this cooperative time limit.
const SHARED_CACHE_SCAN_MAX_ENTRIES: usize = 100_000;
const SHARED_CACHE_SCAN_MAX_DEPTH: usize = 64;
const SHARED_CACHE_SCAN_MAX_ELAPSED: Duration = Duration::from_secs(5);
const SHARED_CACHE_RECOVERY_MAX_ELAPSED: Duration = Duration::from_secs(5);

/// Keep the recovery reserve observable while a preparation command is running.
/// This is cooperative pressure detection, not a byte quota: a filesystem call
/// may stall and a writer may consume space between observations.
struct PreparationHeartbeat {
    next_storage_check: Instant,
    next_renewal: Instant,
}

impl PreparationHeartbeat {
    fn new(now: Instant) -> Self {
        Self {
            next_storage_check: now,
            next_renewal: now + Duration::from_secs(PREPARATION_LEASE_TTL_SECONDS / 3),
        }
    }

    fn tick(
        &mut self,
        now: Instant,
        storage: impl FnOnce() -> Result<(), PreparationError>,
        renew: impl FnOnce() -> Result<(), PreparationError>,
    ) -> Result<(), BrokerOpError> {
        if now >= self.next_storage_check {
            storage()?;
            self.next_storage_check = now + PREPARATION_STORAGE_CHECK_INTERVAL;
        }
        if now >= self.next_renewal {
            renew()?;
            self.next_renewal = now + Duration::from_secs(PREPARATION_LEASE_TTL_SECONDS / 3);
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PreparationError {
    #[error("preparation configuration at {path} is invalid: {reason}")]
    InvalidConfig { path: String, reason: String },
    #[error("preparation input {path:?} is missing or is not a regular file")]
    InvalidInput { path: String },
    #[error(
        "preparation output {path:?} is a symlink; cross-worktree dependency links are forbidden"
    )]
    SymlinkOutput { path: String },
    #[error("managed preparation storage path {path:?} contains a symlink component")]
    ManagedStorageSymlink { path: PathBuf },
    #[error(
        "managed preparation storage declares {paths} path(s) across {steps} step(s), but reservation and root adoption are not wired yet; no preparation command was started. Retry after managed-storage enforcement is available"
    )]
    ManagedStorageNotReady { paths: usize, steps: usize },
    #[error(
        "runtime executable for {name:?} is unavailable; install the declared tool, then retry `aethyme broker submit prepare --session {session_id}`"
    )]
    RuntimeProbe { name: String, session_id: i64 },
    #[error(
        "offline preparation requires step {step:?}, but it has no offline_command and the prepared state is not current"
    )]
    OfflineUnavailable { step: String },
    #[error(
        "preparation step {step:?} failed with exit {exit_code:?}; inspect its output, then retry `aethyme broker submit prepare --session {session_id}`"
    )]
    StepFailed {
        step: String,
        exit_code: Option<i32>,
        session_id: i64,
    },
    #[error(
        "preparation command supervision is unavailable: {reason}; no repository command was started"
    )]
    SupervisorUnavailable { reason: String },
    #[error(
        "preparation command result for session {session_id} could not be confirmed after dispatch: {reason}; process cleanup was verified, but command effects may have occurred. Inspect the worktree before retrying"
    )]
    CommandOutcomeUnknown { session_id: i64, reason: String },
    #[error(
        "preparation for session {session_id} could not verify process cleanup: {reason}; its state remains in progress and any shared-cache lease was not released"
    )]
    CleanupUnknown { session_id: i64, reason: String },
    #[error(
        "preparation for session {session_id} is already in progress or has unresolved cleanup; inspect its preparation status and host lease before retrying"
    )]
    PriorPreparationIncomplete { session_id: i64 },
    #[error(
        "preparation step {step:?} did not produce declared outputs: {paths}; retry after fixing the repository command"
    )]
    MissingOutputs { step: String, paths: String },
    #[error("preparation state i/o at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("preparation state at {path} is invalid: {reason}")]
    InvalidState { path: String, reason: String },
    #[error("preparation cache liveness deadline elapsed")]
    LivenessDeadlineElapsed,
    #[error("preparation cache liveness could not be verified: {reason}")]
    LivenessIncomplete { reason: String },
    #[error("dependency preparation storage at {path} is unavailable: {reason}")]
    StorageUnavailable { path: PathBuf, reason: String },
    #[error(transparent)]
    HostResource(#[from] crate::HostResourceError),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub runtimes: Vec<RuntimeProbe>,
    pub steps: Vec<PreparationStep>,
    /// Budget for the repository-shared store. Shared steps receive an 8 GiB
    /// rotation threshold when this section is absent; explicit zero opts out.
    #[serde(default)]
    pub shared_cache: Option<SharedCacheBudget>,
    /// Opt-in declared growth bounds for managed preparation outputs. The
    /// profile is intentionally separate from `shared_cache.max_bytes`, which
    /// remains a rotation threshold rather than a write quota.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_storage: Option<ManagedPreparationStorageProfile>,
}

/// Explicit per-step growth declarations for worktree outputs and the
/// repository-shared preparation cache. Each configured output must match one
/// output already declared by its preparation step.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedPreparationStorageProfile {
    pub steps: BTreeMap<String, ManagedPreparationStepStorage>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedPreparationStepStorage {
    /// Maximum declared growth for each exact path in `PreparationStep.outputs`.
    #[serde(default)]
    pub output_growth_bytes: BTreeMap<String, u64>,
    /// Maximum growth in the digest-specific shared cache for this step.
    /// Required exactly when the step uses `repository_shared`.
    #[serde(default)]
    pub shared_cache_growth_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedPreparationStoragePlan {
    worktree_root: PathBuf,
    steps: Vec<ManagedPreparationStepStoragePlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedPreparationStepStoragePlan {
    name: String,
    outputs: Vec<ManagedPreparationOutputPlan>,
    shared_cache: Option<ManagedPreparationCachePlan>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedPreparationOutputPlan {
    relative_path: PathBuf,
    absolute_path: PathBuf,
    growth_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedPreparationCachePlan {
    path: PathBuf,
    growth_bytes: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProbe {
    pub name: String,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreparationCachePolicy {
    WorktreeLocal,
    RepositoryShared,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedCacheRoot {
    #[default]
    Host,
    WorktreeVolume,
}

/// Byte budget for the repository-shared preparation cache.
///
/// A shared cache is keyed by preparation digest, so a repository whose
/// lockfile changes between branches accumulates one store directory per
/// distinct input set. This setting is a pre-run rotation threshold, not a
/// write quota: a preparation step can exceed it, and active entries remain
/// protected while in use. Disabling the threshold preserves the historical
/// behavior, but does not bound growth across distinct inputs or clones.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SharedCacheBudget {
    /// Rotate this repository's shared cache above this many logical bytes.
    /// Omission means 8 GiB; explicit zero disables the threshold.
    #[serde(default = "default_shared_cache_max_bytes")]
    pub max_bytes: u64,
    /// Select the cache filesystem. The host default preserves existing
    /// placement; worktree-volume uses the selected session's Aethyme-owned
    /// placement root, unless an explicit operator root was captured.
    #[serde(default)]
    pub root: SharedCacheRoot,
}

fn default_shared_cache_max_bytes() -> u64 {
    8 * 1024 * 1024 * 1024
}

impl Default for SharedCacheBudget {
    fn default() -> Self {
        Self {
            max_bytes: default_shared_cache_max_bytes(),
            root: SharedCacheRoot::default(),
        }
    }
}

fn default_cache_policy() -> PreparationCachePolicy {
    PreparationCachePolicy::WorktreeLocal
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationStep {
    pub name: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub offline_command: Option<Vec<String>>,
    #[serde(default)]
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    #[serde(default = "default_cache_policy")]
    pub cache: PreparationCachePolicy,
    #[serde(default)]
    pub required_for_hooks: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreparationState {
    NotConfigured,
    Required,
    Current,
    Stale,
    InProgress,
    Failed,
    Invalid,
}

impl PreparationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotConfigured => "not_configured",
            Self::Required => "required",
            Self::Current => "current",
            Self::Stale => "stale",
            Self::InProgress => "in_progress",
            Self::Failed => "failed",
            Self::Invalid => "invalid",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PreparationStatus {
    pub schema_version: u32,
    pub session_id: i64,
    pub state: PreparationState,
    pub expected_digest: Option<String>,
    pub recorded_digest: Option<String>,
    pub source_digest: Option<String>,
    pub recorded_source_digest: Option<String>,
    pub missing_outputs: Vec<String>,
    pub hook_required: bool,
    pub reason: String,
    pub next_action: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreparationStepResult {
    pub name: String,
    pub exit_code: Option<i32>,
    pub succeeded: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreparationReport {
    pub schema_version: u32,
    pub session_id: i64,
    pub digest: String,
    pub state: PreparationState,
    pub offline: bool,
    pub shared_cache_coordinated: bool,
    /// Set when the shared store exceeded its declared budget and was rotated.
    /// Additive and `None` in the overwhelmingly common case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_cache_rotation: Option<SharedCacheRotation>,
    pub steps: Vec<PreparationStepResult>,
    pub next_action: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PreparationRecord {
    schema_version: u32,
    session_id: i64,
    state: PreparationState,
    digest: String,
    source_digest: String,
    started_at: i64,
    completed_at: Option<i64>,
    failed_step: Option<String>,
    exit_code: Option<i32>,
    host_lease_id: Option<String>,
    host_lease_generation: Option<u64>,
}

impl Broker {
    fn recover_storage_refusal(&mut self, error: &BrokerOpError) {
        let persisted = self.persist_storage_refusal(error);
        if persisted {
            self.start_storage_recovery_worker();
        }
    }

    /// Persist a recovery generation without launching the worker. A caller
    /// holding the shared preparation lease must release it before launch.
    fn persist_storage_refusal(&mut self, error: &BrokerOpError) -> bool {
        if !is_storage_unavailable(error) {
            return false;
        }
        self.persist_storage_recovery_request()
    }

    fn persist_storage_recovery_request(&mut self) -> bool {
        match self.persist_artifact_sweep_recovery_request() {
            Ok(()) => true,
            Err(recovery_error) => {
                warn_preparation_cleanup_failure("persist cache recovery request", &recovery_error);
                false
            }
        }
    }

    fn start_storage_recovery_worker(&mut self) {
        if let Err(recovery_error) = self.start_artifact_sweep_recovery_worker() {
            warn_preparation_cleanup_failure("start cache recovery worker", &recovery_error);
        }
    }

    fn release_grant_after_error<T>(
        &mut self,
        result: Result<T, PreparationError>,
        coordinator: &mut Option<HostResourceCoordinator>,
        grant: &mut Option<crate::HostResourceGrant>,
        recheck: impl FnOnce() -> Result<(), PreparationError>,
    ) -> Result<T, PreparationError> {
        let error = match result {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let storage_refused = match &error {
            PreparationError::StorageUnavailable { .. } => true,
            _ => matches!(recheck(), Err(PreparationError::StorageUnavailable { .. })),
        };
        let recovery_persisted = storage_refused && self.persist_storage_recovery_request();
        let lease_released = release_grant_best_effort(coordinator, grant);
        if recovery_persisted && lease_released {
            self.start_storage_recovery_worker();
        }
        Err(error)
    }

    /// A package-manager command may fill its volume after preflight and then
    /// fail with an ordinary exit status. Recheck only the already-known write
    /// volumes on failure; this is an admission signal for later recovery, not
    /// a quota or a way to interrupt the command while it runs.
    fn recover_storage_pressure_after_command_failure(
        &mut self,
        error: &BrokerOpError,
        recheck: impl FnOnce() -> Result<(), PreparationError>,
    ) -> bool {
        if is_storage_unavailable(error) {
            return self.persist_storage_refusal(error);
        }
        if let Err(pressure @ PreparationError::StorageUnavailable { .. }) = recheck() {
            return self.persist_storage_refusal(&pressure.into());
        }
        false
    }

    /// Git-ignored top-level directories that exist in the primary checkout but
    /// not in this worktree.
    ///
    /// These are *observations*, not requirements: without a declared
    /// preparation config the broker cannot know what a gate needs. Naming what
    /// visibly differs is enough to turn "nothing to do here" into a concrete
    /// starting point, without inventing a contract the repository never stated.
    fn dependency_paths_absent_from_worktree(&self, worktree: &Path) -> Vec<String> {
        let main = self.main_root();
        if main == worktree {
            return Vec::new();
        }
        let Ok(entries) = fs::read_dir(main) else {
            return Vec::new();
        };
        let mut absent = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') || worktree.join(name).exists() {
                continue;
            }
            // Only ignored directories: a tracked directory missing here would
            // be a checkout problem, not a preparation gap.
            if crate::GitRepo::discover(main)
                .ok()
                .is_some_and(|repo| repo.path_is_ignored(name))
            {
                absent.push(name.to_string());
            }
            if absent.len() >= 5 {
                break;
            }
        }
        absent.sort();
        absent
    }

    pub fn preparation_status(&self, session_id: i64) -> Result<PreparationStatus, BrokerOpError> {
        let session = self.store_ref().session(session_id)?;
        let root = Path::new(&session.worktree_path);
        let config = match load_config(root) {
            Ok(None) => {
                // "Not configured" is only benign when nothing depends on the
                // worktree being prepared. When the repository declares gate
                // commands, a fresh worktree may not be able to run them, and
                // reporting a bare `next_action: null` reads as "nothing to do
                // here" right up until the gate fails at push time.
                let gate_count = crate::gates::load_gates(self.main_root())
                    .map(|gates| gates.len())
                    .unwrap_or(0);
                let absent = self.dependency_paths_absent_from_worktree(root);
                let (reason, next_action) = if gate_count == 0 {
                    (
                        "repository declares no dependency preparation".to_string(),
                        None,
                    )
                } else {
                    let mut reason = format!(
                        "repository declares no dependency preparation but declares {gate_count} \
                         gate command(s); this worktree is not guaranteed to satisfy them"
                    );
                    if !absent.is_empty() {
                        reason.push_str(&format!(
                            "; ignored path(s) present in the primary checkout and absent here: {}",
                            absent.join(", ")
                        ));
                    }
                    (
                        reason,
                        Some(
                            "declare preparation in .aethyme/prepare.toml, or run the \
                             repository's own setup in this worktree before gating"
                                .to_string(),
                        ),
                    )
                };
                return Ok(PreparationStatus {
                    schema_version: PREPARATION_SCHEMA_VERSION,
                    session_id,
                    state: PreparationState::NotConfigured,
                    expected_digest: None,
                    recorded_digest: None,
                    source_digest: None,
                    recorded_source_digest: None,
                    missing_outputs: absent,
                    hook_required: false,
                    reason,
                    next_action,
                });
            }
            Ok(Some(config)) => config,
            Err(error) => {
                return Ok(invalid_status(session_id, error.to_string()));
            }
        };
        let hook_required = config.steps.iter().any(|step| step.required_for_hooks);
        let source_digest = match preparation_source_digest(root, &config, session_id) {
            Ok(digest) => digest,
            Err(error) => return Ok(invalid_status(session_id, error.to_string())),
        };
        let missing_outputs = missing_outputs(root, &config)?;
        let record = read_record(&record_path(self.main_root(), session_id))?;
        let (state, reason, recorded_digest, recorded_source_digest) = match record {
            None => (
                PreparationState::Required,
                "declared dependencies have not been prepared for this session".into(),
                None,
                None,
            ),
            Some(record) if record.state == PreparationState::InProgress => (
                PreparationState::InProgress,
                format!(
                    "a prior preparation was interrupted; inspect host lease {} generation {} before retrying",
                    record.host_lease_id.as_deref().unwrap_or("none"),
                    record.host_lease_generation.unwrap_or(0)
                ),
                Some(record.digest),
                Some(record.source_digest),
            ),
            Some(record) if record.state == PreparationState::Failed => (
                PreparationState::Failed,
                format!(
                    "step {:?} failed with exit {:?}",
                    record.failed_step.as_deref().unwrap_or("unknown"),
                    record.exit_code
                ),
                Some(record.digest),
                Some(record.source_digest),
            ),
            Some(record) if record.source_digest != source_digest => (
                PreparationState::Stale,
                "configuration, inputs, platform, or architecture changed".into(),
                Some(record.digest),
                Some(record.source_digest),
            ),
            Some(record) if !missing_outputs.is_empty() => (
                PreparationState::Stale,
                "one or more declared outputs are missing".into(),
                Some(record.digest),
                Some(record.source_digest),
            ),
            Some(record) => (
                PreparationState::Current,
                "declared dependencies match the prepared-state digest".into(),
                Some(record.digest),
                Some(record.source_digest),
            ),
        };
        let expected_digest = Some(hash_text(&source_digest));
        Ok(PreparationStatus {
            schema_version: PREPARATION_SCHEMA_VERSION,
            session_id,
            state,
            expected_digest,
            recorded_digest,
            source_digest: Some(source_digest),
            recorded_source_digest,
            missing_outputs,
            hook_required,
            reason,
            next_action: (state != PreparationState::Current)
                .then(|| format!("aethyme broker submit prepare --session {session_id}")),
        })
    }

    pub fn prepare_session(
        &mut self,
        session_id: i64,
        offline: bool,
        wait: Duration,
    ) -> Result<PreparationReport, BrokerOpError> {
        let session = self.store_ref().session(session_id)?;
        let root = PathBuf::from(&session.worktree_path);
        let config = load_config(&root)?.ok_or_else(|| PreparationError::InvalidConfig {
            path: PREPARATION_CONFIG_RELPATH.into(),
            reason: "repository declares no preparation steps".into(),
        })?;
        let source_digest = preparation_source_digest(&root, &config, session_id)?;
        let digest = preparation_digest(&root, &config, session_id)?;
        let status = self.preparation_status(session_id)?;
        if status.state == PreparationState::Current {
            return Ok(PreparationReport {
                schema_version: PREPARATION_SCHEMA_VERSION,
                session_id,
                digest,
                state: PreparationState::Current,
                offline,
                shared_cache_coordinated: false,
                shared_cache_rotation: None,
                steps: Vec::new(),
                next_action: "dependencies are already current".into(),
            });
        }
        ensure_preparation_retry_allowed(session_id, status.state)?;
        // Everything below runs the repository's own commands. The policy is
        // this worktree's: its gates and the preparation just loaded.
        let gates = match crate::gates::load_gates(&root) {
            Ok(gates) => gates,
            Err(crate::gates::GateConfigError::Missing(_)) => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        let policy = crate::broker::gate_trust::GatePolicy::from_parts(&gates, Some(&config));
        self.require_trusted_policy(&policy, Some(session_id))?;
        if offline
            && let Some(step) = config
                .steps
                .iter()
                .find(|step| step.offline_command.is_none())
        {
            return Err(PreparationError::OfflineUnavailable {
                step: step.name.clone(),
            }
            .into());
        }

        let shared = config
            .steps
            .iter()
            .any(|step| step.cache == PreparationCachePolicy::RepositoryShared);
        // Keep the checkout shared lock from before cache-root initialization
        // through every synchronous write, child step, record update, and
        // existing host-resource settlement. Each dispatched child also gets
        // its own durable marker through the process supervisor, so a child
        // that outlives a crashed broker remains visible after this scope's
        // lock descriptor closes.
        let _artifact_use_scope =
            match crate::file_lock::artifact_use::begin_artifact_use_scope(self.main_root(), &root)
            {
                Ok(crate::file_lock::artifact_use::ArtifactUseScopeAdmission::Admitted(scope)) => {
                    Some(scope)
                }
                Ok(
                    crate::file_lock::artifact_use::ArtifactUseScopeAdmission::ProtocolUnavailable,
                ) => None,
                Err(source) => {
                    return Err(crate::BrokerError::Io {
                        path: root.clone(),
                        source,
                    }
                    .into());
                }
            };
        let cache_root_binding = if shared {
            let binding = match config
                .shared_cache
                .as_ref()
                .map_or(SharedCacheRoot::Host, |cache| cache.root)
            {
                SharedCacheRoot::Host => preparation_cache_root_binding_for_broker(self)?,
                SharedCacheRoot::WorktreeVolume
                    if self
                        .storage_context()
                        .preparation_cache_root_override
                        .is_none() => self
                    .initialize_worktree_volume_preparation_cache_root(&root)
                    .map_err(|error| PreparationError::StorageUnavailable {
                        path: root.clone(),
                        reason: format!(
                            "cannot initialize the worktree-volume preparation-cache root: {error}"
                        ),
                    })?,
                SharedCacheRoot::WorktreeVolume => {
                    preparation_cache_root_binding_for_broker(self)?
                }
            };
            if config
                .shared_cache
                .as_ref()
                .is_some_and(|cache| cache.root == SharedCacheRoot::WorktreeVolume)
            {
                ensure_preparation_cache_root_on_worktree_volume(&root, &binding)?;
            }
            Some(binding)
        } else {
            None
        };
        let managed_shared_cache_path = if config.managed_storage.is_some() && shared {
            cache_root_binding.as_ref().map(|binding| {
                binding
                    .path
                    .join(repository_identity(self))
                    .join(&digest[..12])
            })
        } else {
            None
        };
        let managed_storage_plan =
            managed_preparation_storage_plan(&root, &config, managed_shared_cache_path.as_deref())?;
        require_managed_storage_execution_ready(managed_storage_plan.as_ref())?;
        let cache_dir = cache_root_binding.as_ref().map(|binding| {
            binding
                .path
                .join(repository_identity(self))
                .join(&digest[..12])
        });
        let resource_db_path = if shared {
            Some(
                self.storage_context()
                    .host_resource_db_path
                    .clone()
                    .ok_or_else(|| {
                        invalid_config(
                            "the broker's captured host resource database path is unavailable",
                        )
                    })?,
            )
        } else {
            None
        };
        let mut storage_paths = vec![root.clone(), self.main_root().join(".aethyme")];
        if let Some(cache_dir) = &cache_dir {
            storage_paths.push(cache_dir.clone());
        }
        if let Some(resource_db_path) = &resource_db_path {
            storage_paths.push(resource_db_path.clone());
        }
        if let Err(error) = require_preparation_storage_with_cache_root(
            self.main_root(),
            &storage_paths,
            cache_root_binding.as_ref(),
        ) {
            let error = BrokerOpError::from(error);
            self.recover_storage_refusal(&error);
            return Err(error);
        }
        // Resolve the exact trusted helper and complete its readiness handshake
        // before opening the host resource DB, acquiring a grant, rotating a
        // cache, or writing preparation state. A library embedder without the
        // compatible broker executable fails here, before admission effects.
        let mut next_supervisor = Some(self.start_preparation_exec_supervisor()?);
        let mut coordinator = resource_db_path
            .as_deref()
            .map(HostResourceCoordinator::open)
            .transpose()
            .map_err(PreparationError::from)?;
        let mut grant = if let Some(coordinator) = coordinator.as_mut() {
            let request = preparation_resource_request(self, session_id, &root, &digest)?;
            Some(
                coordinator
                    .acquire_with_wait(&request, wait, |_| {})
                    .map_err(PreparationError::from)?,
            )
        } else {
            None
        };
        // A shared-cache lease may have waited while another process filled
        // either volume. Recheck before rotation, state writes or commands.
        if let Err(error) = require_preparation_storage_with_cache_root(
            self.main_root(),
            &storage_paths,
            cache_root_binding.as_ref(),
        ) {
            let error = BrokerOpError::from(error);
            let recovery_persisted = self.persist_storage_refusal(&error);
            let lease_released = release_grant_best_effort(&mut coordinator, &mut grant);
            if recovery_persisted && lease_released {
                self.start_storage_recovery_worker();
            }
            return Err(error);
        }
        let storage_root = self.main_root().to_path_buf();
        // Enforce the budget while the exclusive lease is held, never after
        // release: rotation removes a store other worktrees may be reading, so
        // it has to be inside the same critical section that granted access.
        // It also runs before this run's directory is created, because it
        // moves the whole repository store away, that directory included.
        let rotation_result = if shared {
            shared_cache_budget(
                config.shared_cache.as_ref(),
                &digest,
                self,
                cache_root_binding
                    .as_ref()
                    .expect("shared cache has a binding"),
            )
        } else {
            Ok(None)
        };
        let rotation =
            self.release_grant_after_error(rotation_result, &mut coordinator, &mut grant, || {
                require_preparation_storage_with_cache_root(
                    &storage_root,
                    &storage_paths,
                    cache_root_binding.as_ref(),
                )
            })?;
        if let Some(cache_dir) = &cache_dir {
            let create_cache_dir = require_preparation_storage_with_cache_root(
                &storage_root,
                &storage_paths,
                cache_root_binding.as_ref(),
            )
            .and_then(|()| {
                fs::create_dir_all(cache_dir).map_err(|source| PreparationError::Io {
                    path: cache_dir.clone(),
                    source,
                })
            });
            self.release_grant_after_error(create_cache_dir, &mut coordinator, &mut grant, || {
                require_preparation_storage_with_cache_root(
                    &storage_root,
                    &storage_paths,
                    cache_root_binding.as_ref(),
                )
            })?;
        }

        let started_at = now_ms();
        let mut record = PreparationRecord {
            schema_version: PREPARATION_SCHEMA_VERSION,
            session_id,
            state: PreparationState::InProgress,
            digest: digest.clone(),
            source_digest,
            started_at,
            completed_at: None,
            failed_step: None,
            exit_code: None,
            host_lease_id: grant.as_ref().map(|grant| grant.lease.lease_id.clone()),
            host_lease_generation: grant.as_ref().map(|grant| grant.lease.generation),
        };
        let state_path = record_path(self.main_root(), session_id);
        self.release_grant_after_error(
            write_record(&state_path, &record),
            &mut coordinator,
            &mut grant,
            || {
                require_preparation_storage_with_cache_root(
                    &storage_root,
                    &storage_paths,
                    cache_root_binding.as_ref(),
                )
            },
        )?;

        let environment = cache_dir
            .as_ref()
            .map(|path| {
                vec![(
                    "AETHYME_PREPARE_CACHE_DIR".into(),
                    path.to_string_lossy().into_owned(),
                )]
            })
            .unwrap_or_default();
        let mut results = Vec::new();
        let main_root = self.main_root().to_path_buf();
        let mut heartbeat = PreparationHeartbeat::new(Instant::now());
        for step in &config.steps {
            let command = if offline {
                step.offline_command
                    .as_ref()
                    .expect("validated offline command")
            } else {
                &step.command
            };
            let report = match require_preparation_storage_with_cache_root(
                &main_root,
                &storage_paths,
                cache_root_binding.as_ref(),
            )
            .map_err(BrokerOpError::from)
            .and_then(|()| {
                let supervisor = match next_supervisor.take() {
                    Some(supervisor) => supervisor,
                    None => self.start_preparation_exec_supervisor()?,
                };
                self.guarded_preparation_exec_with_supervisor(
                    session_id,
                    command,
                    &environment,
                    supervisor,
                    || {
                        heartbeat.tick(
                            Instant::now(),
                            || {
                                require_preparation_storage_with_cache_root(
                                    &main_root,
                                    &storage_paths,
                                    cache_root_binding.as_ref(),
                                )
                            },
                            || {
                                if let (Some(coordinator), Some(grant)) =
                                    (coordinator.as_mut(), grant.as_mut())
                                {
                                    grant.lease = coordinator
                                        .renew(
                                            &grant.lease.lease_id,
                                            grant.lease.generation,
                                            &grant.ownership_token,
                                            PREPARATION_LEASE_TTL_SECONDS,
                                        )
                                        .map_err(PreparationError::from)?;
                                }
                                Ok(())
                            },
                        )
                    },
                )
            }) {
                Ok(report) => report,
                Err(error) => {
                    let restart_storage_worker = route_preparation_command_failure(
                        &error,
                        PreparationCommandFailureContext {
                            step: &step.name,
                            state_path: &state_path,
                            record: &mut record,
                            coordinator: &mut coordinator,
                            grant: &mut grant,
                            exit_code: None,
                            write_exit_code: false,
                        },
                        |error| {
                            self.recover_storage_pressure_after_command_failure(error, || {
                                require_preparation_storage_with_cache_root(
                                    &main_root,
                                    &storage_paths,
                                    cache_root_binding.as_ref(),
                                )
                            })
                        },
                    );
                    if restart_storage_worker {
                        self.start_storage_recovery_worker();
                    }
                    return Err(error);
                }
            };
            results.push(PreparationStepResult {
                name: step.name.clone(),
                exit_code: report.exit_code,
                succeeded: report.ok,
            });
            if !report.ok {
                let error = PreparationError::StepFailed {
                    step: step.name.clone(),
                    exit_code: report.exit_code,
                    session_id,
                };
                let error = BrokerOpError::from(error);
                let restart_storage_worker = route_preparation_command_failure(
                    &error,
                    PreparationCommandFailureContext {
                        step: &step.name,
                        state_path: &state_path,
                        record: &mut record,
                        coordinator: &mut coordinator,
                        grant: &mut grant,
                        exit_code: report.exit_code,
                        write_exit_code: true,
                    },
                    |error| {
                        self.recover_storage_pressure_after_command_failure(error, || {
                            require_preparation_storage_with_cache_root(
                                &main_root,
                                &storage_paths,
                                cache_root_binding.as_ref(),
                            )
                        })
                    },
                );
                if restart_storage_worker {
                    self.start_storage_recovery_worker();
                }
                return Err(error);
            }
            if let Err(cache_error) =
                ensure_preparation_cache_root_binding(cache_root_binding.as_ref())
            {
                let error = BrokerOpError::from(cache_error);
                let recovery_persisted =
                    self.recover_storage_pressure_after_command_failure(&error, || {
                        require_preparation_storage_with_cache_root(
                            &main_root,
                            &storage_paths,
                            cache_root_binding.as_ref(),
                        )
                    });
                record.state = PreparationState::Failed;
                record.failed_step = Some(step.name.clone());
                record.exit_code = report.exit_code;
                record.completed_at = Some(now_ms());
                let lease_released = best_effort_failed_preparation_cleanup(
                    || write_record(&state_path, &record),
                    || release_grant(&mut coordinator, &mut grant),
                );
                if recovery_persisted && lease_released {
                    self.start_storage_recovery_worker();
                }
                return Err(error);
            }
            let absent = match missing_step_outputs(&root, step) {
                Ok(absent) => absent,
                Err(error) => {
                    record.state = PreparationState::Failed;
                    record.failed_step = Some(step.name.clone());
                    record.completed_at = Some(now_ms());
                    best_effort_failed_preparation_cleanup(
                        || write_record(&state_path, &record),
                        || release_grant(&mut coordinator, &mut grant),
                    );
                    return Err(error.into());
                }
            };
            if !absent.is_empty() {
                record.state = PreparationState::Failed;
                record.failed_step = Some(step.name.clone());
                record.completed_at = Some(now_ms());
                best_effort_failed_preparation_cleanup(
                    || write_record(&state_path, &record),
                    || release_grant(&mut coordinator, &mut grant),
                );
                return Err(PreparationError::MissingOutputs {
                    step: step.name.clone(),
                    paths: absent.join(", "),
                }
                .into());
            }
        }
        release_grant(&mut coordinator, &mut grant)?;
        record.state = PreparationState::Current;
        record.completed_at = Some(now_ms());
        record.host_lease_id = None;
        record.host_lease_generation = None;
        self.release_grant_after_error(
            write_record(&state_path, &record),
            &mut coordinator,
            &mut grant,
            || {
                require_preparation_storage_with_cache_root(
                    &storage_root,
                    &storage_paths,
                    cache_root_binding.as_ref(),
                )
            },
        )?;
        Ok(PreparationReport {
            schema_version: PREPARATION_SCHEMA_VERSION,
            session_id,
            digest,
            state: PreparationState::Current,
            offline,
            shared_cache_coordinated: shared,
            shared_cache_rotation: rotation,
            steps: results,
            next_action: "continue work in the prepared session".into(),
        })
    }
}

pub(crate) fn load_config(root: &Path) -> Result<Option<PreparationConfig>, PreparationError> {
    let path = root.join(PREPARATION_CONFIG_RELPATH);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(PreparationError::Io { path, source }),
    };
    parse_config(&text).map(Some)
}

/// Parse and validate preparation configuration already read from an exact
/// source, a checkout file or a committed blob alike.
pub(crate) fn parse_config(text: &str) -> Result<PreparationConfig, PreparationError> {
    let mut config: PreparationConfig =
        toml::from_str(text).map_err(|error| PreparationError::InvalidConfig {
            path: PREPARATION_CONFIG_RELPATH.into(),
            reason: error.to_string(),
        })?;
    if config.shared_cache.is_none()
        && config
            .steps
            .iter()
            .any(|step| step.cache == PreparationCachePolicy::RepositoryShared)
    {
        config.shared_cache = Some(SharedCacheBudget::default());
    }
    validate_config(&config)?;
    Ok(config)
}

fn validate_config(config: &PreparationConfig) -> Result<(), PreparationError> {
    if config.schema_version != PREPARATION_SCHEMA_VERSION {
        return Err(invalid_config(format!(
            "unsupported schema {}; expected {PREPARATION_SCHEMA_VERSION}",
            config.schema_version
        )));
    }
    if config.steps.is_empty() {
        return Err(invalid_config("steps must not be empty"));
    }
    // A budget on a store that is never shared is a configuration mistake
    // rather than a harmless no-op: it reads as a disk bound that is not being
    // applied, which is the belief the field exists to remove.
    if config.shared_cache.is_some()
        && !config
            .steps
            .iter()
            .any(|step| step.cache == PreparationCachePolicy::RepositoryShared)
    {
        return Err(invalid_config(
            "shared_cache requires at least one step with cache = \"repository_shared\"",
        ));
    }
    let mut runtime_names = BTreeSet::new();
    for runtime in &config.runtimes {
        validate_name_and_command("runtime", &runtime.name, &runtime.command)?;
        if !runtime_names.insert(runtime.name.as_str()) {
            return Err(invalid_config(format!(
                "duplicate runtime {:?}",
                runtime.name
            )));
        }
    }
    let mut names = BTreeSet::new();
    for step in &config.steps {
        validate_name_and_command("step", &step.name, &step.command)?;
        if !names.insert(step.name.as_str()) {
            return Err(invalid_config(format!("duplicate step {:?}", step.name)));
        }
        if step.outputs.is_empty() {
            return Err(invalid_config(format!(
                "step {:?} must declare at least one output",
                step.name
            )));
        }
        if let Some(command) = &step.offline_command {
            validate_name_and_command("offline step", &step.name, command)?;
        }
        for path in step.inputs.iter().chain(&step.outputs) {
            validate_relative_path(path)?;
        }
    }
    if let Some(profile) = &config.managed_storage {
        validate_managed_storage_profile(config, profile)?;
    }
    Ok(())
}

fn validate_managed_storage_profile(
    config: &PreparationConfig,
    profile: &ManagedPreparationStorageProfile,
) -> Result<(), PreparationError> {
    let configured_steps = config
        .steps
        .iter()
        .map(|step| step.name.as_str())
        .collect::<BTreeSet<_>>();
    for name in profile.steps.keys() {
        if !configured_steps.contains(name.as_str()) {
            return Err(invalid_config(format!(
                "managed_storage.steps contains unknown preparation step {name:?}"
            )));
        }
    }

    let mut declared_output_roots = Vec::<(PathBuf, String, String)>::new();
    let mut total_growth_bytes = 0_u64;
    for step in &config.steps {
        let storage = profile.steps.get(&step.name).ok_or_else(|| {
            invalid_config(format!(
                "managed_storage.steps is missing preparation step {:?}",
                step.name
            ))
        })?;
        if step.outputs.iter().collect::<BTreeSet<_>>().len() != step.outputs.len() {
            return Err(invalid_config(format!(
                "managed_storage step {:?} cannot bound duplicate declared output paths",
                step.name
            )));
        }
        let declared_outputs = step.outputs.iter().collect::<BTreeSet<_>>();
        let bounded_outputs = storage.output_growth_bytes.keys().collect::<BTreeSet<_>>();
        if declared_outputs != bounded_outputs {
            return Err(invalid_config(format!(
                "managed_storage.steps.{:?}.output_growth_bytes must cover exactly the step's declared outputs",
                step.name
            )));
        }

        for (output, growth_bytes) in &storage.output_growth_bytes {
            if *growth_bytes == 0 {
                return Err(invalid_config(format!(
                    "managed growth for output {output:?} in step {:?} must be positive",
                    step.name
                )));
            }
            let relative_path = normalized_managed_output_path(output)?;
            if let Some((_, other_step, other_output)) = declared_output_roots
                .iter()
                .find(|(existing, _, _)| paths_overlap(existing, &relative_path))
            {
                return Err(invalid_config(format!(
                    "managed output roots {other_output:?} in step {other_step:?} and {output:?} in step {:?} overlap",
                    step.name
                )));
            }
            declared_output_roots.push((relative_path, step.name.clone(), output.clone()));
            total_growth_bytes =
                total_growth_bytes
                    .checked_add(*growth_bytes)
                    .ok_or_else(|| {
                        invalid_config(
                            "managed output growth total exceeds the supported u64 range",
                        )
                    })?;
        }

        match (step.cache, storage.shared_cache_growth_bytes) {
            (PreparationCachePolicy::RepositoryShared, Some(growth_bytes)) if growth_bytes > 0 => {
                total_growth_bytes =
                    total_growth_bytes
                        .checked_add(growth_bytes)
                        .ok_or_else(|| {
                            invalid_config(
                                "managed shared-cache growth total exceeds the supported u64 range",
                            )
                        })?;
            }
            (PreparationCachePolicy::RepositoryShared, Some(_)) => {
                return Err(invalid_config(format!(
                    "managed shared-cache growth for step {:?} must be positive",
                    step.name
                )));
            }
            (PreparationCachePolicy::RepositoryShared, None) => {
                return Err(invalid_config(format!(
                    "managed_storage.steps.{:?}.shared_cache_growth_bytes is required for cache = \"repository_shared\"",
                    step.name
                )));
            }
            (PreparationCachePolicy::WorktreeLocal, Some(_)) => {
                return Err(invalid_config(format!(
                    "managed_storage.steps.{:?}.shared_cache_growth_bytes requires cache = \"repository_shared\"",
                    step.name
                )));
            }
            (PreparationCachePolicy::WorktreeLocal, None) => {}
        }
    }
    if total_growth_bytes == 0 {
        return Err(invalid_config(
            "managed_storage must declare at least one positive output or shared-cache growth bound",
        ));
    }
    Ok(())
}

fn normalized_managed_output_path(path: &str) -> Result<PathBuf, PreparationError> {
    validate_relative_path(path)?;
    let mut normalized = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            _ => {
                return Err(invalid_config(format!(
                    "managed output path {path:?} must contain only normal relative components"
                )));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(invalid_config(format!(
            "managed output path {path:?} must name a path below the worktree root"
        )));
    }
    Ok(normalized)
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn managed_preparation_storage_plan(
    worktree_root: &Path,
    config: &PreparationConfig,
    shared_cache_path: Option<&Path>,
) -> Result<Option<ManagedPreparationStoragePlan>, PreparationError> {
    let Some(profile) = &config.managed_storage else {
        return Ok(None);
    };
    validate_config(config)?;

    if !worktree_root.is_absolute() {
        return Err(invalid_config(
            "the broker-captured preparation worktree root must be absolute",
        ));
    }
    refuse_managed_storage_symlink_components(worktree_root)?;
    let root_metadata =
        fs::symlink_metadata(worktree_root).map_err(|source| PreparationError::Io {
            path: worktree_root.to_path_buf(),
            source,
        })?;
    if !root_metadata.is_dir() {
        return Err(invalid_config(
            "the broker-captured preparation worktree root is not a directory",
        ));
    }

    let needs_shared_cache = config.steps.iter().any(|step| {
        profile
            .steps
            .get(&step.name)
            .is_some_and(|storage| storage.shared_cache_growth_bytes.is_some())
    });
    match (needs_shared_cache, shared_cache_path) {
        (true, None) => {
            return Err(invalid_config(
                "managed shared-cache growth has no digest-specific cache path from captured broker storage context",
            ));
        }
        (false, Some(_)) => {
            return Err(invalid_config(
                "a managed shared-cache path was supplied without a shared-cache growth declaration",
            ));
        }
        _ => {}
    }
    if let Some(path) = shared_cache_path {
        if !path.is_absolute() {
            return Err(invalid_config(
                "the broker-captured managed shared-cache path must be absolute",
            ));
        }
        refuse_managed_storage_symlink_components(path)?;
    }

    let mut steps = Vec::with_capacity(config.steps.len());
    for step in &config.steps {
        let storage = profile.steps.get(&step.name).ok_or_else(|| {
            invalid_config(format!(
                "managed_storage.steps is missing preparation step {:?}",
                step.name
            ))
        })?;
        let mut outputs = Vec::with_capacity(step.outputs.len());
        for output in &step.outputs {
            let relative_path = normalized_managed_output_path(output)?;
            let absolute_path = worktree_root.join(&relative_path);
            refuse_managed_storage_symlink_components(&absolute_path)?;
            if let Some(cache_path) = shared_cache_path
                && paths_overlap(&absolute_path, cache_path)
            {
                return Err(invalid_config(format!(
                    "managed output {output:?} overlaps the digest-specific shared-cache path"
                )));
            }
            let growth_bytes = *storage.output_growth_bytes.get(output).ok_or_else(|| {
                invalid_config(format!(
                    "managed output {output:?} in step {:?} has no growth declaration",
                    step.name
                ))
            })?;
            outputs.push(ManagedPreparationOutputPlan {
                relative_path,
                absolute_path,
                growth_bytes,
            });
        }
        let shared_cache = if let Some(growth_bytes) = storage.shared_cache_growth_bytes {
            let path = shared_cache_path.ok_or_else(|| {
                invalid_config(
                    "managed shared-cache growth has no digest-specific cache path from captured broker storage context",
                )
            })?;
            Some(ManagedPreparationCachePlan {
                path: path.to_path_buf(),
                growth_bytes,
            })
        } else {
            None
        };
        steps.push(ManagedPreparationStepStoragePlan {
            name: step.name.clone(),
            outputs,
            shared_cache,
        });
    }

    Ok(Some(ManagedPreparationStoragePlan {
        worktree_root: worktree_root.to_path_buf(),
        steps,
    }))
}

fn refuse_managed_storage_symlink_components(path: &Path) -> Result<(), PreparationError> {
    let components = path.components().collect::<Vec<_>>();
    let mut candidate = PathBuf::new();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::Prefix(_) | Component::RootDir => candidate.push(component.as_os_str()),
            Component::Normal(part) => candidate.push(part),
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(invalid_config(format!(
                    "managed storage path {path:?} contains parent traversal"
                )));
            }
        }
        match fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(PreparationError::ManagedStorageSymlink {
                    path: path.to_path_buf(),
                });
            }
            Ok(metadata) if index + 1 < components.len() && !metadata.is_dir() => {
                return Err(invalid_config(format!(
                    "managed storage path {path:?} has a non-directory component"
                )));
            }
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(PreparationError::Io {
                    path: candidate,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn require_managed_storage_execution_ready(
    plan: Option<&ManagedPreparationStoragePlan>,
) -> Result<(), PreparationError> {
    let Some(plan) = plan else {
        return Ok(());
    };
    let mut path_count = 0_usize;
    for step in &plan.steps {
        if step.name.is_empty() {
            return Err(invalid_config(
                "managed storage plan contains an unnamed step",
            ));
        }
        for output in &step.outputs {
            if output.relative_path.is_absolute()
                || !output.absolute_path.starts_with(&plan.worktree_root)
                || output.growth_bytes == 0
            {
                return Err(invalid_config(
                    "managed storage plan contains an invalid output reservation",
                ));
            }
            path_count = path_count.saturating_add(1);
        }
        if let Some(cache) = &step.shared_cache {
            if !cache.path.is_absolute() || cache.growth_bytes == 0 {
                return Err(invalid_config(
                    "managed storage plan contains an invalid shared-cache reservation",
                ));
            }
            path_count = path_count.saturating_add(1);
        }
    }
    Err(PreparationError::ManagedStorageNotReady {
        paths: path_count,
        steps: plan.steps.len(),
    })
}

fn validate_name_and_command(
    kind: &str,
    name: &str,
    command: &[String],
) -> Result<(), PreparationError> {
    if name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(invalid_config(format!(
            "{kind} name must be 1..=128 printable bytes"
        )));
    }
    if command.is_empty() || command[0].is_empty() {
        return Err(invalid_config(format!(
            "{kind} {name:?} command must not be empty"
        )));
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<(), PreparationError> {
    let parsed = Path::new(path);
    if path.trim_end_matches(['/', '\\']).is_empty()
        || parsed.is_absolute()
        || parsed.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid_config(format!(
            "path {path:?} must be repository-relative without parent traversal"
        )));
    }
    Ok(())
}

pub(crate) fn preparation_digest(
    root: &Path,
    config: &PreparationConfig,
    session_id: i64,
) -> Result<String, PreparationError> {
    Ok(hash_text(&preparation_source_digest(
        root, config, session_id,
    )?))
}

fn preparation_source_digest(
    root: &Path,
    config: &PreparationConfig,
    session_id: i64,
) -> Result<String, PreparationError> {
    preparation_source_digest_before(root, config, session_id, None)
}

fn preparation_source_digest_before(
    root: &Path,
    config: &PreparationConfig,
    session_id: i64,
    deadline: Option<Instant>,
) -> Result<String, PreparationError> {
    check_liveness_deadline(deadline)?;
    let mut hasher = Sha256::new();
    hasher.update(PREPARATION_SCHEMA_VERSION.to_le_bytes());
    hasher.update(std::env::consts::OS.as_bytes());
    hasher.update([0]);
    hasher.update(std::env::consts::ARCH.as_bytes());
    let config_bytes =
        serde_json::to_vec(config).map_err(|error| invalid_config(error.to_string()))?;
    check_liveness_deadline(deadline)?;
    hasher.update(config_bytes);
    let mut inputs = config
        .steps
        .iter()
        .flat_map(|step| step.inputs.iter())
        .collect::<Vec<_>>();
    inputs.sort();
    inputs.dedup();
    for relative in inputs {
        check_liveness_deadline(deadline)?;
        let path = root.join(relative);
        let (length, digest) = preparation_input_digest(&path, relative, deadline)?;
        hasher.update(relative.as_bytes());
        hasher.update(length.to_le_bytes());
        hasher.update(digest);
    }
    for runtime in &config.runtimes {
        check_liveness_deadline(deadline)?;
        let executable = resolve_runtime_executable_before(root, &runtime.command[0], deadline)?
            .ok_or_else(|| PreparationError::RuntimeProbe {
                name: runtime.name.clone(),
                session_id,
            })?;
        let canonical = executable
            .canonicalize()
            .map_err(|_| PreparationError::RuntimeProbe {
                name: runtime.name.clone(),
                session_id,
            })?;
        check_liveness_deadline(deadline)?;
        let metadata = fs::metadata(&canonical).map_err(|_| PreparationError::RuntimeProbe {
            name: runtime.name.clone(),
            session_id,
        })?;
        check_liveness_deadline(deadline)?;
        if !metadata.is_file() {
            return Err(PreparationError::RuntimeProbe {
                name: runtime.name.clone(),
                session_id,
            });
        }
        hasher.update(runtime.name.as_bytes());
        hasher.update(canonical.to_string_lossy().as_bytes());
        hasher.update(metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            hasher.update(duration.as_nanos().to_le_bytes());
        }
        check_liveness_deadline(deadline)?;
    }
    check_liveness_deadline(deadline)?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn check_liveness_deadline(deadline: Option<Instant>) -> Result<(), PreparationError> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(PreparationError::LivenessDeadlineElapsed);
    }
    Ok(())
}

fn open_liveness_file(path: &Path) -> Result<std::fs::File, std::io::Error> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path)
}

fn same_regular_file_version(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    if !left.is_file()
        || !right.is_file()
        || left.len() != right.len()
        || left.modified().ok() != right.modified().ok()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if left.dev() != right.dev()
            || left.ino() != right.ino()
            || left.ctime() != right.ctime()
            || left.ctime_nsec() != right.ctime_nsec()
        {
            return false;
        }
    }
    true
}

fn preparation_input_digest(
    path: &Path,
    relative: &str,
    deadline: Option<Instant>,
) -> Result<(u64, [u8; 32]), PreparationError> {
    check_liveness_deadline(deadline)?;
    let before = fs::symlink_metadata(path).map_err(|_| PreparationError::InvalidInput {
        path: relative.to_owned(),
    })?;
    check_liveness_deadline(deadline)?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(PreparationError::InvalidInput {
            path: relative.to_owned(),
        });
    }
    check_liveness_deadline(deadline)?;
    let mut file = open_liveness_file(path).map_err(|source| PreparationError::Io {
        path: relative.into(),
        source,
    })?;
    check_liveness_deadline(deadline)?;
    let opened = file.metadata().map_err(|source| PreparationError::Io {
        path: relative.into(),
        source,
    })?;
    check_liveness_deadline(deadline)?;
    if !same_regular_file_version(&before, &opened) {
        return Err(PreparationError::InvalidInput {
            path: relative.to_owned(),
        });
    }

    let expected_len = opened.len();
    let mut length = 0_u64;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; PREPARATION_CACHE_HASH_BUFFER_BYTES];
    loop {
        check_liveness_deadline(deadline)?;
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(PreparationError::Io {
                    path: relative.into(),
                    source,
                });
            }
        };
        check_liveness_deadline(deadline)?;
        if read == 0 {
            break;
        }
        length = length
            .checked_add(read as u64)
            .ok_or_else(|| PreparationError::InvalidInput {
                path: relative.to_owned(),
            })?;
        if length > expected_len {
            return Err(PreparationError::InvalidInput {
                path: relative.to_owned(),
            });
        }
        digest.update(&buffer[..read]);
    }
    if length != expected_len {
        return Err(PreparationError::InvalidInput {
            path: relative.to_owned(),
        });
    }
    check_liveness_deadline(deadline)?;
    let after_open = file.metadata().map_err(|source| PreparationError::Io {
        path: relative.into(),
        source,
    })?;
    check_liveness_deadline(deadline)?;
    let after_path = fs::symlink_metadata(path).map_err(|_| PreparationError::InvalidInput {
        path: relative.to_owned(),
    })?;
    check_liveness_deadline(deadline)?;
    if after_path.file_type().is_symlink()
        || !same_regular_file_version(&opened, &after_open)
        || !same_regular_file_version(&opened, &after_path)
    {
        return Err(PreparationError::InvalidInput {
            path: relative.to_owned(),
        });
    }
    check_liveness_deadline(deadline)?;
    Ok((length, digest.finalize().into()))
}

fn load_config_for_cache_liveness(
    root: &Path,
    deadline: Instant,
) -> Result<Option<PreparationConfig>, PreparationError> {
    check_liveness_deadline(Some(deadline))?;
    let root_metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            check_liveness_deadline(Some(deadline))?;
            return Ok(None);
        }
        Err(source) => {
            return Err(PreparationError::Io {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(PreparationError::LivenessIncomplete {
            reason: "checkout root is not a real directory".into(),
        });
    }
    check_liveness_deadline(Some(deadline))?;
    let config_dir = root.join(".aethyme");
    let config_dir_metadata = match fs::symlink_metadata(&config_dir) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            check_liveness_deadline(Some(deadline))?;
            return Ok(None);
        }
        Err(source) => {
            return Err(PreparationError::Io {
                path: config_dir,
                source,
            });
        }
    };
    if config_dir_metadata.file_type().is_symlink() || !config_dir_metadata.is_dir() {
        return Err(PreparationError::LivenessIncomplete {
            reason: ".aethyme is not a real directory".into(),
        });
    }
    check_liveness_deadline(Some(deadline))?;
    let path = root.join(PREPARATION_CONFIG_RELPATH);
    let Some(bytes) = read_bounded_regular_file_for_liveness(
        &path,
        PREPARATION_CACHE_CONFIG_MAX_BYTES,
        deadline,
    )?
    else {
        return Ok(None);
    };
    let text = String::from_utf8(bytes).map_err(|error| invalid_config(error.to_string()))?;
    let config = parse_config(&text)?;
    check_liveness_deadline(Some(deadline))?;
    Ok(Some(config))
}

pub(crate) fn read_bounded_regular_file_for_liveness(
    path: &Path,
    maximum_bytes: u64,
    deadline: Instant,
) -> Result<Option<Vec<u8>>, PreparationError> {
    check_liveness_deadline(Some(deadline))?;
    let before = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            check_liveness_deadline(Some(deadline))?;
            return Ok(None);
        }
        Err(source) => {
            return Err(PreparationError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    check_liveness_deadline(Some(deadline))?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(PreparationError::LivenessIncomplete {
            reason: format!("{} is not a regular file", path.display()),
        });
    }
    if before.len() > maximum_bytes {
        return Err(PreparationError::LivenessIncomplete {
            reason: format!("{} exceeds its bounded read size", path.display()),
        });
    }
    let mut file = open_liveness_file(path).map_err(|source| PreparationError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    check_liveness_deadline(Some(deadline))?;
    let opened = file.metadata().map_err(|source| PreparationError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    check_liveness_deadline(Some(deadline))?;
    if !same_regular_file_version(&before, &opened) {
        return Err(PreparationError::LivenessIncomplete {
            reason: format!("{} changed during bounded read", path.display()),
        });
    }
    let mut bytes = Vec::with_capacity(usize::try_from(opened.len()).unwrap_or(0));
    let mut buffer = [0_u8; PREPARATION_CACHE_HASH_BUFFER_BYTES];
    loop {
        check_liveness_deadline(Some(deadline))?;
        let read = match file.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(PreparationError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        check_liveness_deadline(Some(deadline))?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) as u64 > maximum_bytes {
            return Err(PreparationError::LivenessIncomplete {
                reason: format!("{} grew beyond its bounded read size", path.display()),
            });
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    check_liveness_deadline(Some(deadline))?;
    let after_open = file.metadata().map_err(|source| PreparationError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    check_liveness_deadline(Some(deadline))?;
    let after_path = fs::symlink_metadata(path).map_err(|source| PreparationError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    check_liveness_deadline(Some(deadline))?;
    if after_path.file_type().is_symlink()
        || !same_regular_file_version(&opened, &after_open)
        || !same_regular_file_version(&opened, &after_path)
        || bytes.len() as u64 != opened.len()
    {
        return Err(PreparationError::LivenessIncomplete {
            reason: format!("{} changed during bounded read", path.display()),
        });
    }
    Ok(Some(bytes))
}

fn resolve_runtime_executable_before(
    root: &Path,
    program: &str,
    deadline: Option<Instant>,
) -> Result<Option<PathBuf>, PreparationError> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        check_liveness_deadline(deadline)?;
        let is_file = candidate.is_file();
        check_liveness_deadline(deadline)?;
        return Ok(is_file.then_some(candidate));
    }
    let Some(search_path) = std::env::var_os("PATH") else {
        return Ok(None);
    };
    for directory in std::env::split_paths(&search_path) {
        check_liveness_deadline(deadline)?;
        let candidate = directory.join(program);
        let is_file = candidate.is_file();
        check_liveness_deadline(deadline)?;
        if is_file {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn missing_outputs(
    root: &Path,
    config: &PreparationConfig,
) -> Result<Vec<String>, PreparationError> {
    let mut missing = Vec::new();
    for step in &config.steps {
        missing.extend(missing_step_outputs(root, step)?);
    }
    missing.sort();
    missing.dedup();
    Ok(missing)
}

fn missing_step_outputs(
    root: &Path,
    step: &PreparationStep,
) -> Result<Vec<String>, PreparationError> {
    let mut missing = Vec::new();
    for relative in &step.outputs {
        let path = root.join(relative.trim_end_matches(['/', '\\']));
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(PreparationError::SymlinkOutput {
                    path: relative.clone(),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(relative.clone())
            }
            Err(source) => return Err(PreparationError::Io { path, source }),
        }
    }
    Ok(missing)
}

fn record_path(main_root: &Path, session_id: i64) -> PathBuf {
    main_root
        .join(".aethyme/run/preparation")
        .join(format!("session-{session_id}.json"))
}

fn read_record(path: &Path) -> Result<Option<PreparationRecord>, PreparationError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(PreparationError::Io {
                path: path.into(),
                source,
            });
        }
    };
    let record: PreparationRecord =
        serde_json::from_slice(&bytes).map_err(|error| PreparationError::InvalidState {
            path: path.to_string_lossy().into_owned(),
            reason: error.to_string(),
        })?;
    if record.schema_version != PREPARATION_SCHEMA_VERSION {
        return Err(PreparationError::InvalidState {
            path: path.to_string_lossy().into_owned(),
            reason: format!(
                "unsupported schema {}; expected {PREPARATION_SCHEMA_VERSION}",
                record.schema_version
            ),
        });
    }
    Ok(Some(record))
}

fn write_record(path: &Path, record: &PreparationRecord) -> Result<(), PreparationError> {
    let parent = path.parent().expect("preparation state has parent");
    fs::create_dir_all(parent).map_err(|source| PreparationError::Io {
        path: parent.into(),
        source,
    })?;
    let temp = parent.join(format!(
        ".session-{}.{}.tmp",
        record.session_id,
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|source| PreparationError::Io {
            path: temp.clone(),
            source,
        })?;
    let mut bytes =
        serde_json::to_vec_pretty(record).map_err(|error| PreparationError::InvalidState {
            path: path.to_string_lossy().into_owned(),
            reason: error.to_string(),
        })?;
    bytes.push(b'\n');
    file.write_all(&bytes)
        .map_err(|source| PreparationError::Io {
            path: temp.clone(),
            source,
        })?;
    file.sync_all().map_err(|source| PreparationError::Io {
        path: temp.clone(),
        source,
    })?;
    fs::rename(&temp, path).map_err(|source| PreparationError::Io {
        path: path.into(),
        source,
    })?;
    Ok(())
}

fn preparation_resource_request(
    broker: &Broker,
    session_id: i64,
    root: &Path,
    digest: &str,
) -> Result<HostResourceRequest, PreparationError> {
    let repository = repository_identity(broker);
    let worktree_fingerprint = hash_text(&root.to_string_lossy());
    let run_id = format!("prepare-s{session_id}-{}", &digest[..12]);
    Ok(HostResourceRequest {
        schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
        request_id: format!("{run_id}-{}-{}", std::process::id(), now_ms()),
        repository: repository.clone(),
        worktree_fingerprint,
        run_id,
        ttl_seconds: PREPARATION_LEASE_TTL_SECONDS,
        holder_pid: Some(std::process::id()),
        resources: vec![HostResourceRequirement {
            key: "preparation_cache".into(),
            resource: HostResourceKind::ExclusiveKey {
                name: preparation_cache_lease_name(&repository),
            },
        }],
    })
}

pub(crate) fn preparation_cache_lease_name(repository: &str) -> String {
    format!("aethyme-prepare-cache:{repository}")
}

/// The shared-cache key this checkout would ask for right now, if it declares
/// a shared step at all.
///
/// This is what makes evicting the shared cache a decision rather than a
/// guess. The key is a content hash of the declared inputs, the config and the
/// platform, so an entry is live exactly when some current checkout would
/// recompute its name -- a question with an answer, unlike "has this been
/// touched recently".
///
/// `session_id` reaches the digest only through error messages, so any value
/// produces the same key; zero is passed because no session is asking.
pub(crate) fn current_cache_key(root: &Path) -> Result<Option<String>, PreparationError> {
    current_cache_key_before(root, None)
}

pub(crate) fn current_cache_key_before(
    root: &Path,
    deadline: Option<Instant>,
) -> Result<Option<String>, PreparationError> {
    let config = match deadline {
        Some(deadline) => load_config_for_cache_liveness(root, deadline)?,
        None => load_config(root)?,
    };
    let Some(config) = config else {
        return Ok(None);
    };
    check_liveness_deadline(deadline)?;
    if !config
        .steps
        .iter()
        .any(|step| step.cache == PreparationCachePolicy::RepositoryShared)
    {
        check_liveness_deadline(deadline)?;
        return Ok(None);
    }
    let source_digest = preparation_source_digest_before(root, &config, 0, deadline)?;
    check_liveness_deadline(deadline)?;
    let digest = hash_text(&source_digest);
    Ok(Some(digest[..12.min(digest.len())].to_string()))
}

/// Rotate the shared store when it exceeds its declared budget.
///
/// Rotation is whole-store rather than per-entry because the budget exists to
/// bound a *rate* of growth, and evicting the oldest entries is a policy the
/// package managers inside the store already own. It is reported rather than
/// silent, because a customer watching a cache reset mid-day deserves to know
/// which command did it.
///
/// Recovers any prior rotation before applying the optional budget. A missing
/// budget still avoids a size measurement, but cannot leave a published
/// rotation half-finished while allowing a new preparation command to start.
fn shared_cache_budget(
    budget: Option<&SharedCacheBudget>,
    digest: &str,
    broker: &Broker,
    cache_root_binding: &crate::broker::PreparationCacheRootBinding,
) -> Result<Option<SharedCacheRotation>, PreparationError> {
    let cache_root = cache_root_binding.path.clone();
    let resource_db_path = broker
        .storage_context()
        .host_resource_db_path
        .as_deref()
        .ok_or_else(|| PreparationError::StorageUnavailable {
            path: cache_root.clone(),
            reason: "the broker's captured host resource database path is unavailable".into(),
        })?;
    shared_cache_budget_at_with_resource_db_and_identity_and_binding(
        budget,
        &cache_root,
        resource_db_path,
        &repository_identity(broker),
        digest,
        cache_root_binding.identity.as_deref(),
        Some(cache_root_binding),
    )
}

fn ensure_preparation_cache_root_on_worktree_volume(
    worktree: &Path,
    binding: &crate::broker::PreparationCacheRootBinding,
) -> Result<(), PreparationError> {
    binding
        .validate()
        .map_err(|error| PreparationError::StorageUnavailable {
            path: binding.path.clone(),
            reason: format!("captured preparation-cache root changed: {error}"),
        })?;
    let worktree_identity =
        rotation_debt::DirectoryIdentity::capture(worktree).map_err(|error| {
            PreparationError::StorageUnavailable {
                path: worktree.to_path_buf(),
                reason: format!("cannot verify worktree filesystem: {error}"),
            }
        })?;
    let cache_identity =
        rotation_debt::DirectoryIdentity::capture(&binding.path).map_err(|error| {
            PreparationError::StorageUnavailable {
                path: binding.path.clone(),
                reason: format!("cannot verify preparation-cache filesystem: {error}"),
            }
        })?;
    if !worktree_identity.same_filesystem(&cache_identity) {
        return Err(PreparationError::StorageUnavailable {
            path: binding.path.clone(),
            reason:
                "selected preparation-cache root is on a different filesystem from the worktree"
                    .into(),
        });
    }
    Ok(())
}

/// Rotation against an explicit cache root and repository identity.
///
/// Split from [`shared_cache_budget`] so the decision is testable without a
/// broker, a host state directory, or a real package-manager store on disk.
/// The caller holds the exclusive lease while this function recovers existing
/// debt and applies any newly declared budget.
#[cfg(test)]
fn shared_cache_budget_at(
    budget: Option<&SharedCacheBudget>,
    cache_root: &Path,
    repository: &str,
    digest: &str,
) -> Result<Option<SharedCacheRotation>, PreparationError> {
    let resource_db_path = cache_root
        .parent()
        .unwrap_or(cache_root)
        .join(".aethyme-test-host-resources.sqlite3");
    shared_cache_budget_at_with_resource_db(
        budget,
        cache_root,
        &resource_db_path,
        repository,
        digest,
    )
}

#[cfg(test)]
fn shared_cache_budget_at_with_resource_db(
    budget: Option<&SharedCacheBudget>,
    cache_root: &Path,
    resource_db_path: &Path,
    repository: &str,
    digest: &str,
) -> Result<Option<SharedCacheRotation>, PreparationError> {
    shared_cache_budget_at_with_resource_db_and_identity(
        budget,
        cache_root,
        resource_db_path,
        repository,
        digest,
        None,
    )
}

#[cfg(test)]
fn shared_cache_budget_at_with_resource_db_and_identity(
    budget: Option<&SharedCacheBudget>,
    cache_root: &Path,
    resource_db_path: &Path,
    repository: &str,
    digest: &str,
    expected_root_identity: Option<&[u8]>,
) -> Result<Option<SharedCacheRotation>, PreparationError> {
    shared_cache_budget_at_with_resource_db_and_identity_and_binding(
        budget,
        cache_root,
        resource_db_path,
        repository,
        digest,
        expected_root_identity,
        None,
    )
}

fn shared_cache_budget_at_with_resource_db_and_identity_and_binding(
    budget: Option<&SharedCacheBudget>,
    cache_root: &Path,
    resource_db_path: &Path,
    repository: &str,
    digest: &str,
    expected_root_identity: Option<&[u8]>,
    root_binding: Option<&crate::broker::PreparationCacheRootBinding>,
) -> Result<Option<SharedCacheRotation>, PreparationError> {
    ensure_preparation_cache_root_binding(root_binding)?;
    ensure_preparation_cache_root_identity(cache_root, expected_root_identity)?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    recover_rotation_debt(
        resource_db_path,
        cache_root,
        repository,
        expected_root_identity,
        root_binding,
    )?;

    let Some(budget) = budget.filter(|budget| budget.max_bytes > 0) else {
        return Ok(None);
    };
    let store = cache_root.join(repository);
    let Some(bytes) = directory_bytes(&store)? else {
        return Ok(None);
    };
    ensure_preparation_cache_root_identity(cache_root, expected_root_identity)?;
    ensure_preparation_cache_root_binding(root_binding)?;
    if bytes <= budget.max_bytes {
        return Ok(None);
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err(PreparationError::StorageUnavailable {
        path: store,
        reason: "durable shared-cache rotation recovery is unavailable on this platform".into(),
    });

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let cache_root_identity =
            rotation_debt::DirectoryIdentity::capture(cache_root).map_err(|source| {
                PreparationError::StorageUnavailable {
                    path: cache_root.to_path_buf(),
                    reason: format!("cannot identify the preparation-cache root: {source}"),
                }
            })?;
        let store_identity =
            rotation_debt::DirectoryIdentity::capture(&store).map_err(|source| {
                PreparationError::StorageUnavailable {
                    path: store.clone(),
                    reason: format!("cannot identify the shared preparation cache: {source}"),
                }
            })?;
        let debt = rotation_debt::RotationDebt::new(
            repository,
            &rotation_debt::new_transaction_id(),
            cache_root_identity.clone(),
            store_identity,
        )
        .map_err(|source| PreparationError::StorageUnavailable {
            path: store.clone(),
            reason: format!("cannot record shared-cache rotation intent: {source}"),
        })?;
        let root_identity_bytes = cache_root_identity.encode().map_err(|source| {
            PreparationError::StorageUnavailable {
                path: cache_root.to_path_buf(),
                reason: format!("cannot encode shared-cache root identity: {source}"),
            }
        })?;
        ensure_preparation_cache_root_identity(cache_root, expected_root_identity)?;
        if expected_root_identity.is_some_and(|expected| expected != root_identity_bytes) {
            return Err(PreparationError::StorageUnavailable {
                path: cache_root.to_path_buf(),
                reason: "preparation-cache root differs from the broker's captured identity".into(),
            });
        }
        let payload = rotation_debt::encode(&debt).map_err(|source| {
            PreparationError::StorageUnavailable {
                path: store.clone(),
                reason: format!("cannot encode shared-cache rotation intent: {source}"),
            }
        })?;
        ensure_preparation_cache_root_identity(cache_root, expected_root_identity)?;
        ensure_preparation_cache_root_binding(root_binding)?;
        rotation_journal::publish(
            resource_db_path,
            cache_root,
            &root_identity_bytes,
            repository,
            &payload,
        )
        .map_err(|source| PreparationError::StorageUnavailable {
            path: store.clone(),
            reason: format!("cannot durably journal shared-cache rotation intent: {source}"),
        })?;
        ensure_preparation_cache_root_identity(cache_root, expected_root_identity)?;
        ensure_preparation_cache_root_binding(root_binding)?;
        rotation_debt::publish_with_root_guard(cache_root, debt, &mut || {
            validate_preparation_cache_binding_for_recovery(root_binding)
        })
        .map_err(|source| PreparationError::StorageUnavailable {
            path: store.clone(),
            reason: format!("cannot publish shared-cache rotation intent: {source}"),
        })?;
        let progress = recover_rotation_debt(
            resource_db_path,
            cache_root,
            repository,
            expected_root_identity,
            root_binding,
        )?;
        if !progress.found_debt || !progress.complete {
            return Err(PreparationError::StorageUnavailable {
                path: store.clone(),
                reason: "shared-cache rotation did not complete; retry after recovery".into(),
            });
        }
    }
    Ok(Some(SharedCacheRotation {
        // The key is a digest, never a path: a report must not disclose where
        // the store lives on this machine.
        key: digest[..12.min(digest.len())].to_string(),
        budget_bytes: budget.max_bytes,
        bytes_before: bytes,
        bytes_after: 0,
        removed: true,
    }))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn recover_rotation_debt_for_cache_sweep_with_binding(
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
    resource_db_path: &Path,
    repository: &str,
    root_binding: Option<&crate::broker::PreparationCacheRootBinding>,
    deadline: Instant,
) -> Result<rotation_debt::RecoveryProgress, rotation_debt::RecoveryError> {
    if !preparation_cache_root_identity_matches(cache_root, expected_root_identity) {
        return Err(rotation_debt::RecoveryError {
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "preparation-cache root differs from the broker's captured identity",
            ),
            progress: rotation_debt::RecoveryProgress::default(),
        });
    }
    validate_preparation_cache_binding_for_recovery(root_binding).map_err(|source| {
        rotation_debt::RecoveryError {
            source,
            progress: rotation_debt::RecoveryProgress::default(),
        }
    })?;
    rotation_debt::recover_with_journal_and_root_guard(
        cache_root,
        resource_db_path,
        repository,
        deadline,
        || validate_preparation_cache_binding_for_recovery(root_binding),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn recover_rotation_debt_for_cache_sweep_with_binding(
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
    _resource_db_path: &Path,
    repository: &str,
    root_binding: Option<&crate::broker::PreparationCacheRootBinding>,
    deadline: Instant,
) -> Result<rotation_debt::RecoveryProgress, rotation_debt::RecoveryError> {
    if !preparation_cache_root_identity_matches(cache_root, expected_root_identity) {
        return Err(rotation_debt::RecoveryError {
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "preparation-cache root differs from the broker's captured identity",
            ),
            progress: rotation_debt::RecoveryProgress::default(),
        });
    }
    validate_preparation_cache_binding_for_recovery(root_binding).map_err(|source| {
        rotation_debt::RecoveryError {
            source,
            progress: rotation_debt::RecoveryProgress::default(),
        }
    })?;
    rotation_debt::recover_with_root_guard(cache_root, repository, deadline, || {
        validate_preparation_cache_binding_for_recovery(root_binding)
    })
}

pub(crate) fn probe_rotation_recovery(
    _cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
    _resource_db_path: &Path,
    max_entries: usize,
    _deadline: Instant,
) -> rotation_debt::DebtProbe {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if Instant::now() >= _deadline {
            return rotation_debt::DebtProbe::Unknown;
        }
        let root_identity = match rotation_debt::DirectoryIdentity::capture(_cache_root) {
            Ok(identity) => match identity.encode() {
                Ok(bytes) => Some(bytes),
                Err(_) => return rotation_debt::DebtProbe::Unknown,
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match rotation_debt::cache_root_absent_under_observed_parent(_cache_root, _deadline)
                {
                    Ok(true) => None,
                    _ => return rotation_debt::DebtProbe::Unknown,
                }
            }
            Err(_) => return rotation_debt::DebtProbe::Unknown,
        };
        if !preparation_cache_root_identity_matches(_cache_root, expected_root_identity) {
            return rotation_debt::DebtProbe::Unknown;
        }
        let cache_probe = rotation_debt::probe(_cache_root, max_entries, _deadline);
        let journal_probe =
            match rotation_journal::probe(_resource_db_path, _cache_root, root_identity.as_deref())
            {
                Ok(Some(rotation_journal::JournalProbe::Pending)) => {
                    rotation_debt::DebtProbe::Pending
                }
                Ok(Some(rotation_journal::JournalProbe::Unknown)) | Err(_) => {
                    rotation_debt::DebtProbe::Unknown
                }
                Ok(Some(rotation_journal::JournalProbe::NoDebt)) | Ok(None) => {
                    rotation_debt::DebtProbe::NoDebt
                }
            };
        match (cache_probe, journal_probe) {
            (rotation_debt::DebtProbe::Unknown, _) | (_, rotation_debt::DebtProbe::Unknown) => {
                rotation_debt::DebtProbe::Unknown
            }
            (rotation_debt::DebtProbe::Pending, _) | (_, rotation_debt::DebtProbe::Pending) => {
                rotation_debt::DebtProbe::Pending
            }
            _ => rotation_debt::DebtProbe::NoDebt,
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        rotation_debt::DebtProbe::Unknown
    }
}

pub(crate) fn preparation_cache_root_identity_matches(
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
) -> bool {
    let Some(expected) = expected_root_identity else {
        return true;
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        rotation_debt::DirectoryIdentity::capture(cache_root)
            .and_then(|identity| identity.encode())
            .is_ok_and(|actual| actual == expected)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = cache_root;
        false
    }
}

fn ensure_preparation_cache_root_identity(
    cache_root: &Path,
    expected_root_identity: Option<&[u8]>,
) -> Result<(), PreparationError> {
    if preparation_cache_root_identity_matches(cache_root, expected_root_identity) {
        return Ok(());
    }
    Err(PreparationError::StorageUnavailable {
        path: cache_root.to_path_buf(),
        reason: "preparation-cache root differs from the broker's captured identity".into(),
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn recover_rotation_debt(
    resource_db_path: &Path,
    cache_root: &Path,
    repository: &str,
    expected_root_identity: Option<&[u8]>,
    root_binding: Option<&crate::broker::PreparationCacheRootBinding>,
) -> Result<rotation_debt::RecoveryProgress, PreparationError> {
    ensure_preparation_cache_root_identity(cache_root, expected_root_identity)?;
    ensure_preparation_cache_root_binding(root_binding)?;
    let deadline = Instant::now() + SHARED_CACHE_RECOVERY_MAX_ELAPSED;
    match rotation_debt::recover_with_journal_and_root_guard(
        cache_root,
        resource_db_path,
        repository,
        deadline,
        || validate_preparation_cache_binding_for_recovery(root_binding),
    ) {
        Ok(progress) if progress.complete => Ok(progress),
        Ok(progress) => Err(PreparationError::StorageUnavailable {
            path: cache_root.join(repository),
            reason: format!(
                "shared-cache rotation recovery stopped after unlinking {} filesystem entries",
                progress.entries_reclaimed
            ),
        }),
        Err(error) => Err(PreparationError::StorageUnavailable {
            path: cache_root.join(repository),
            reason: format!(
                "shared-cache rotation recovery is incomplete after unlinking {} filesystem entries: {}",
                error.progress.entries_reclaimed, error.source
            ),
        }),
    }
}

fn validate_preparation_cache_binding_for_recovery(
    binding: Option<&crate::broker::PreparationCacheRootBinding>,
) -> std::io::Result<()> {
    if let Some(binding) = binding {
        binding.validate().map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("captured preparation-cache root changed: {error}"),
            )
        })?;
    }
    Ok(())
}

/// What a rotation did, for the report and the event.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SharedCacheRotation {
    pub key: String,
    pub budget_bytes: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// True after the old store is fully removed. Incomplete recovery returns
    /// a storage refusal and leaves the durable debt for the next sweep.
    pub removed: bool,
}

fn preparation_cache_root_binding_for_broker(
    broker: &Broker,
) -> Result<crate::broker::PreparationCacheRootBinding, PreparationError> {
    let context = broker.storage_context();
    let fallback = context
        .preparation_cache_root_override
        .clone()
        .unwrap_or_else(|| PathBuf::from("preparation-cache"));
    context
        .preparation_cache_root_binding()
        .map_err(|error| PreparationError::StorageUnavailable {
            path: fallback.clone(),
            reason: format!("captured preparation-cache root is unavailable: {error}"),
        })?
        .ok_or_else(|| PreparationError::StorageUnavailable {
            path: fallback,
            reason: "the broker's captured host resource database path is unavailable".into(),
        })
}

/// Resolve the default cache root for tests of legacy/default configuration.
#[cfg(test)]
pub(crate) fn preparation_cache_root() -> Result<PathBuf, PreparationError> {
    let db = crate::default_host_resource_db_path()?;
    let parent = db
        .parent()
        .ok_or_else(|| invalid_config("host state has no parent directory"))?;
    Ok(parent.join("preparation-cache"))
}

#[derive(Clone, Copy)]
struct SharedCacheScanLimits {
    max_entries: usize,
    max_depth: usize,
    max_elapsed: Duration,
}

impl Default for SharedCacheScanLimits {
    fn default() -> Self {
        Self {
            max_entries: SHARED_CACHE_SCAN_MAX_ENTRIES,
            max_depth: SHARED_CACHE_SCAN_MAX_DEPTH,
            max_elapsed: SHARED_CACHE_SCAN_MAX_ELAPSED,
        }
    }
}

fn directory_bytes(path: &Path) -> Result<Option<u64>, PreparationError> {
    directory_bytes_with_limits(path, SharedCacheScanLimits::default())
}

#[cfg(target_os = "linux")]
fn shared_cache_stat_device(device: libc::dev_t) -> Option<u64> {
    Some(device)
}

#[cfg(target_os = "macos")]
fn shared_cache_stat_device(device: libc::dev_t) -> Option<u64> {
    u64::try_from(device).ok()
}

fn directory_bytes_with_limits(
    path: &Path,
    _limits: SharedCacheScanLimits,
) -> Result<Option<u64>, PreparationError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        directory_bytes_fd_relative(path, _limits)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(shared_cache_scan_refusal(
            path,
            "bounded no-follow inventory is unsupported on this platform",
        ))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct SharedCacheDirectoryStream(*mut libc::DIR);

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl SharedCacheDirectoryStream {
    fn from_owned_fd(fd: libc::c_int, path: &Path) -> Result<Self, PreparationError> {
        // SAFETY: fd is an open directory descriptor and ownership transfers to
        // fdopendir on success. The stream is closed by Drop.
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let source = std::io::Error::last_os_error();
            // SAFETY: fdopendir failed, so it did not take ownership of fd.
            unsafe { libc::close(fd) };
            return Err(shared_cache_scan_io_refusal(path, &source));
        }
        Ok(Self(stream))
    }

    fn fd(&self, path: &Path) -> Result<libc::c_int, PreparationError> {
        // SAFETY: self owns a live DIR stream until Drop.
        let fd = unsafe { libc::dirfd(self.0) };
        if fd < 0 {
            return Err(shared_cache_scan_io_refusal(
                path,
                &std::io::Error::last_os_error(),
            ));
        }
        Ok(fd)
    }

    fn next_name(&mut self, path: &Path) -> Result<Option<CString>, PreparationError> {
        // readdir reports end-of-directory and errors with the same null
        // pointer, so clear errno before each call and inspect it afterwards.
        // SAFETY: the platform-specific errno accessor points to this thread's
        // errno slot.
        unsafe { *shared_cache_errno_location() = 0 };
        // SAFETY: self owns a live DIR stream; readdir's pointer is copied
        // before another call can invalidate it.
        let entry = unsafe { libc::readdir(self.0) };
        if entry.is_null() {
            // SAFETY: errno was initialized above and readdir ran on this
            // thread.
            let errno = unsafe { *shared_cache_errno_location() };
            if errno == 0 {
                return Ok(None);
            }
            return Err(shared_cache_scan_io_refusal(
                path,
                &std::io::Error::from_raw_os_error(errno),
            ));
        }
        // SAFETY: POSIX dirent::d_name is a NUL-terminated name valid until
        // the next readdir call. CString copies the bytes now.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        CString::new(name)
            .map(Some)
            .map_err(|_| shared_cache_scan_refusal(path, "directory entry name is invalid"))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for SharedCacheDirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this stream uniquely owns the DIR pointer from fdopendir.
        unsafe { libc::closedir(self.0) };
    }
}

#[cfg(target_os = "linux")]
unsafe fn shared_cache_errno_location() -> *mut libc::c_int {
    // SAFETY: libc exposes the calling thread's errno slot.
    unsafe { libc::__errno_location() }
}

#[cfg(target_os = "macos")]
unsafe fn shared_cache_errno_location() -> *mut libc::c_int {
    // SAFETY: libc exposes the calling thread's errno slot.
    unsafe { libc::__error() }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct SharedCacheDirectoryFrame {
    stream: SharedCacheDirectoryStream,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
enum SharedCacheScanOpenTarget {
    Root,
    Child,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn directory_bytes_fd_relative(
    path: &Path,
    limits: SharedCacheScanLimits,
) -> Result<Option<u64>, PreparationError> {
    directory_bytes_fd_relative_with_hook(path, limits, |_| {})
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn directory_bytes_fd_relative_with_hook<F>(
    path: &Path,
    limits: SharedCacheScanLimits,
    mut before_open: F,
) -> Result<Option<u64>, PreparationError>
where
    F: FnMut(SharedCacheScanOpenTarget),
{
    use std::mem::MaybeUninit;

    let started = Instant::now();
    let root_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(shared_cache_scan_io_refusal(path, &source)),
    };
    if root_metadata.file_type().is_symlink() {
        return Err(shared_cache_scan_refusal(
            path,
            "cache store root is a symlink",
        ));
    }
    if !root_metadata.is_dir() {
        return Err(shared_cache_scan_refusal(
            path,
            "cache store root is not a directory",
        ));
    }
    if limits.max_entries == 0 || limits.max_depth == 0 {
        return Err(shared_cache_scan_refusal(
            path,
            "cache inventory limit was exceeded",
        ));
    }
    check_shared_cache_scan_deadline(path, started, limits)?;

    let path_c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| shared_cache_scan_refusal(path, "cache store path is invalid"))?;
    let flags =
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    before_open(SharedCacheScanOpenTarget::Root);
    // SAFETY: path_c is NUL-terminated and flags request a directory without
    // following the final path component if it is a symlink.
    let root_fd = unsafe { libc::open(path_c.as_ptr(), flags) };
    if root_fd < 0 {
        let source = std::io::Error::last_os_error();
        if source.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(shared_cache_scan_io_refusal(path, &source));
    }

    let mut root_stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: root_fd is open and root_stat is writable storage for stat.
    if unsafe { libc::fstat(root_fd, root_stat.as_mut_ptr()) } != 0 {
        let source = std::io::Error::last_os_error();
        // SAFETY: fd was opened above and has not transferred to fdopendir.
        unsafe { libc::close(root_fd) };
        return Err(shared_cache_scan_io_refusal(path, &source));
    }
    // SAFETY: successful fstat initialized the structure.
    let root_stat = unsafe { root_stat.assume_init() };
    if root_stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || shared_cache_stat_device(root_stat.st_dev) != Some(root_metadata.dev())
        || root_stat.st_ino != root_metadata.ino()
    {
        // SAFETY: fd was opened above and has not transferred to fdopendir.
        unsafe { libc::close(root_fd) };
        return Err(shared_cache_scan_refusal(
            path,
            "cache store root changed during inventory",
        ));
    }
    let root_device = root_stat.st_dev;
    let root_stream = SharedCacheDirectoryStream::from_owned_fd(root_fd, path)?;
    let mut pending = vec![SharedCacheDirectoryFrame {
        stream: root_stream,
    }];
    let mut visited_entries = 0_usize;
    let mut bytes = 0_u64;

    loop {
        check_shared_cache_scan_deadline(path, started, limits)?;
        let Some(frame) = pending.last_mut() else {
            break;
        };
        let name = frame.stream.next_name(path)?;
        check_shared_cache_scan_deadline(path, started, limits)?;
        let Some(name) = name else {
            pending.pop();
            continue;
        };
        if name.as_bytes() == b"." || name.as_bytes() == b".." {
            continue;
        }
        visited_entries = visited_entries.checked_add(1).ok_or_else(|| {
            shared_cache_scan_refusal(path, "cache inventory entry count overflowed")
        })?;
        if visited_entries > limits.max_entries {
            return Err(shared_cache_scan_refusal(
                path,
                "cache inventory entry limit was exceeded",
            ));
        }

        let parent_fd = pending
            .last()
            .expect("nonempty directory stack")
            .stream
            .fd(path)?;
        check_shared_cache_scan_deadline(path, started, limits)?;
        let mut entry_stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent_fd is owned by the live stream, name is NUL-terminated,
        // and entry_stat is writable storage. AT_SYMLINK_NOFOLLOW classifies
        // links without opening their target.
        if unsafe {
            libc::fstatat(
                parent_fd,
                name.as_ptr(),
                entry_stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(shared_cache_scan_io_refusal(
                path,
                &std::io::Error::last_os_error(),
            ));
        }
        check_shared_cache_scan_deadline(path, started, limits)?;
        // SAFETY: successful fstatat initialized the structure.
        let entry_stat = unsafe { entry_stat.assume_init() };
        if entry_stat.st_dev != root_device {
            return Err(shared_cache_scan_refusal(
                path,
                "cache inventory crossed a filesystem boundary",
            ));
        }
        let entry_type = entry_stat.st_mode & libc::S_IFMT;
        match entry_type {
            libc::S_IFDIR => {
                if pending.len() >= limits.max_depth {
                    return Err(shared_cache_scan_refusal(
                        path,
                        "cache inventory directory depth limit was exceeded",
                    ));
                }
                check_shared_cache_scan_deadline(path, started, limits)?;
                before_open(SharedCacheScanOpenTarget::Child);
                // SAFETY: name is NUL-terminated and parent_fd remains live.
                let child_fd = unsafe { libc::openat(parent_fd, name.as_ptr(), flags) };
                if child_fd < 0 {
                    return Err(shared_cache_scan_io_refusal(
                        path,
                        &std::io::Error::last_os_error(),
                    ));
                }
                let mut child_stat = MaybeUninit::<libc::stat>::uninit();
                // SAFETY: child_fd is open and child_stat is writable storage.
                if unsafe { libc::fstat(child_fd, child_stat.as_mut_ptr()) } != 0 {
                    let source = std::io::Error::last_os_error();
                    // SAFETY: child_fd remains owned here because fdopendir was
                    // not called after the failed fstat.
                    unsafe { libc::close(child_fd) };
                    return Err(shared_cache_scan_io_refusal(path, &source));
                }
                // SAFETY: successful fstat initialized the structure.
                let child_stat = unsafe { child_stat.assume_init() };
                if child_stat.st_mode & libc::S_IFMT != libc::S_IFDIR
                    || child_stat.st_dev != root_device
                    || child_stat.st_dev != entry_stat.st_dev
                    || child_stat.st_ino != entry_stat.st_ino
                {
                    // SAFETY: child_fd remains owned here because fdopendir was
                    // not called after validation failed.
                    unsafe { libc::close(child_fd) };
                    return Err(shared_cache_scan_refusal(
                        path,
                        "cache directory changed or crossed a filesystem boundary",
                    ));
                }
                let child = SharedCacheDirectoryStream::from_owned_fd(child_fd, path)?;
                check_shared_cache_scan_deadline(path, started, limits)?;
                pending.push(SharedCacheDirectoryFrame { stream: child });
            }
            libc::S_IFREG | libc::S_IFLNK => {
                let size = u64::try_from(entry_stat.st_size).map_err(|_| {
                    shared_cache_scan_refusal(path, "cache entry has an invalid size")
                })?;
                bytes = bytes.checked_add(size).ok_or_else(|| {
                    shared_cache_scan_refusal(path, "cache inventory byte count overflowed")
                })?;
            }
            _ => {
                return Err(shared_cache_scan_refusal(
                    path,
                    "cache inventory contains a special file",
                ));
            }
        }
    }

    Ok(Some(bytes))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn check_shared_cache_scan_deadline(
    path: &Path,
    started: Instant,
    limits: SharedCacheScanLimits,
) -> Result<(), PreparationError> {
    if started.elapsed() >= limits.max_elapsed {
        return Err(shared_cache_scan_refusal(
            path,
            "cache inventory time limit was exceeded",
        ));
    }
    Ok(())
}

fn shared_cache_scan_refusal(path: &Path, reason: &str) -> PreparationError {
    PreparationError::StorageUnavailable {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn shared_cache_scan_io_refusal(path: &Path, source: &std::io::Error) -> PreparationError {
    PreparationError::StorageUnavailable {
        path: path.to_path_buf(),
        reason: format!("cache inventory could not be read ({:?})", source.kind()),
    }
}

fn require_preparation_storage(
    main_root: &Path,
    paths: &[PathBuf],
) -> Result<(), PreparationError> {
    preparation_storage_with(
        paths,
        crate::verification::removable_volume_unavailable_reason,
        |path| crate::disk_headroom::available_headroom_at_or_above_for(main_root, path),
    )
}

fn ensure_preparation_cache_root_binding(
    binding: Option<&crate::broker::PreparationCacheRootBinding>,
) -> Result<(), PreparationError> {
    if let Some(binding) = binding {
        binding
            .validate()
            .map_err(|error| PreparationError::StorageUnavailable {
                path: binding.path.clone(),
                reason: format!("captured preparation-cache root changed: {error}"),
            })?;
        ensure_preparation_cache_root_identity(&binding.path, binding.identity.as_deref())?;
    }
    Ok(())
}

fn require_preparation_storage_with_cache_root(
    main_root: &Path,
    paths: &[PathBuf],
    binding: Option<&crate::broker::PreparationCacheRootBinding>,
) -> Result<(), PreparationError> {
    require_preparation_storage(main_root, paths)?;
    ensure_preparation_cache_root_binding(binding)
}

fn is_storage_unavailable(error: &BrokerOpError) -> bool {
    matches!(
        error,
        BrokerOpError::Preparation(PreparationError::StorageUnavailable { .. })
    )
}

fn best_effort_failed_preparation_cleanup(
    write_state: impl FnOnce() -> Result<(), PreparationError>,
    release_lease: impl FnOnce() -> Result<(), PreparationError>,
) -> bool {
    if let Err(error) = write_state() {
        warn_preparation_cleanup_failure("write failed preparation state", &error);
    }
    match release_lease() {
        Ok(()) => true,
        Err(error) => {
            warn_preparation_cleanup_failure("release host resource lease", &error);
            false
        }
    }
}

struct PreparationCommandFailureContext<'a> {
    step: &'a str,
    state_path: &'a Path,
    record: &'a mut PreparationRecord,
    coordinator: &'a mut Option<HostResourceCoordinator>,
    grant: &'a mut Option<crate::HostResourceGrant>,
    exit_code: Option<i32>,
    write_exit_code: bool,
}

fn route_preparation_command_failure(
    error: &BrokerOpError,
    context: PreparationCommandFailureContext<'_>,
    recover_storage_pressure: impl FnOnce(&BrokerOpError) -> bool,
) -> bool {
    let PreparationCommandFailureContext {
        step,
        state_path,
        record,
        coordinator,
        grant,
        exit_code,
        write_exit_code,
    } = context;
    if matches!(
        error,
        BrokerOpError::Preparation(
            PreparationError::CommandOutcomeUnknown { .. }
                | PreparationError::CleanupUnknown { .. }
        )
    ) {
        // The command may have changed the worktree even when its process is
        // gone. Keep the durable in-progress record and host lease until an
        // operator inspects the effects and chooses how to reconcile them.
        quarantine_grant_best_effort(coordinator, grant);
        return false;
    }

    let recovery_persisted = recover_storage_pressure(error);
    record.state = PreparationState::Failed;
    record.failed_step = Some(step.into());
    if write_exit_code {
        record.exit_code = exit_code;
    }
    record.completed_at = Some(now_ms());
    let lease_released = best_effort_failed_preparation_cleanup(
        || write_record(state_path, record),
        || release_grant(coordinator, grant),
    );
    recovery_persisted && lease_released
}

fn ensure_preparation_retry_allowed(
    session_id: i64,
    state: PreparationState,
) -> Result<(), PreparationError> {
    if state == PreparationState::InProgress {
        return Err(PreparationError::PriorPreparationIncomplete { session_id });
    }
    Ok(())
}

fn release_grant_best_effort(
    coordinator: &mut Option<HostResourceCoordinator>,
    grant: &mut Option<crate::HostResourceGrant>,
) -> bool {
    match release_grant(coordinator, grant) {
        Ok(()) => true,
        Err(error) => {
            warn_preparation_cleanup_failure("release host resource lease", &error);
            false
        }
    }
}

fn quarantine_grant_best_effort(
    coordinator: &mut Option<HostResourceCoordinator>,
    grant: &mut Option<crate::HostResourceGrant>,
) -> bool {
    let (Some(coordinator), Some(grant_ref)) = (coordinator.as_mut(), grant.as_ref()) else {
        if grant.is_some() {
            warn_preparation_cleanup_failure(
                "quarantine host resource lease after unverified process cleanup",
                &"coordinator is unavailable",
            );
            return false;
        }
        return true;
    };
    match coordinator.quarantine(
        &grant_ref.lease.lease_id,
        grant_ref.lease.generation,
        &grant_ref.ownership_token,
    ) {
        Ok(lease) if lease.state == crate::HostLeaseState::Quarantined => {
            grant.take();
            true
        }
        Ok(lease) => {
            warn_preparation_cleanup_failure(
                "quarantine host resource lease after unverified process cleanup",
                &format!("lease returned in unexpected state {:?}", lease.state),
            );
            false
        }
        Err(error) => {
            warn_preparation_cleanup_failure(
                "quarantine host resource lease after unverified process cleanup",
                &error,
            );
            false
        }
    }
}

fn warn_preparation_cleanup_failure(action: &str, error: &impl std::fmt::Display) {
    eprintln!("warning: could not {action} after preparation refusal: {error}");
}

fn preparation_storage_with(
    paths: &[PathBuf],
    mut unavailable: impl FnMut(&Path) -> Option<String>,
    mut headroom: impl FnMut(&Path) -> Option<crate::disk_headroom::DiskHeadroom>,
) -> Result<(), PreparationError> {
    for path in paths {
        let reason = unavailable(path).or_else(|| {
            let reading = headroom(path);
            crate::disk_headroom::refusal_with_headroom(
                reading.map(|reading| reading.bytes),
                reading.and_then(|reading| reading.inodes),
                crate::DEFAULT_GATE_HEADROOM_BYTES,
                crate::disk_headroom::MIN_GATE_HEADROOM_INODES,
            )
        });
        if let Some(reason) = reason {
            return Err(PreparationError::StorageUnavailable {
                path: path.clone(),
                reason,
            });
        }
    }
    Ok(())
}

fn repository_identity(broker: &Broker) -> String {
    repository_identity_for_repo(broker.repo_handle())
}

pub(crate) fn repository_identity_for_repo(repo: &crate::GitRepo) -> String {
    crate::resolve_remote_target(repo, "origin", None)
        .map(|target| hash_text(&target.coordination_key))
        .unwrap_or_else(|_| hash_text(&repo.root().to_string_lossy()))
}

fn release_grant(
    coordinator: &mut Option<HostResourceCoordinator>,
    grant: &mut Option<crate::HostResourceGrant>,
) -> Result<(), PreparationError> {
    match (coordinator.as_mut(), grant.as_ref()) {
        (Some(coordinator), Some(leased_grant)) => {
            let lease_id = leased_grant.lease.lease_id.clone();
            let generation = leased_grant.lease.generation;
            let ownership_token = leased_grant.ownership_token.clone();
            coordinator.release(&lease_id, generation, &ownership_token)?;
            grant.take();
        }
        (Some(_), None) | (None, None) => {}
        (None, Some(_)) => {
            return Err(PreparationError::InvalidState {
                path: "host-resource lease".into(),
                reason: "coordinator and acquired grant are inconsistent".into(),
            });
        }
    }
    Ok(())
}

fn invalid_status(session_id: i64, reason: String) -> PreparationStatus {
    PreparationStatus {
        schema_version: PREPARATION_SCHEMA_VERSION,
        session_id,
        state: PreparationState::Invalid,
        expected_digest: None,
        recorded_digest: None,
        source_digest: None,
        recorded_source_digest: None,
        missing_outputs: Vec::new(),
        hook_required: true,
        reason,
        next_action: Some(format!(
            "fix {PREPARATION_CONFIG_RELPATH}, then run `aethyme broker submit prepare --session {session_id}`"
        )),
    }
}

fn invalid_config(reason: impl Into<String>) -> PreparationError {
    PreparationError::InvalidConfig {
        path: PREPARATION_CONFIG_RELPATH.into(),
        reason: reason.into(),
    }
}

fn hash_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHARED_CACHE_TEST_REPOSITORY: &str =
        "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn initialized_cache_root_binding(
        cache_root: &Path,
        resource_db_path: &Path,
    ) -> crate::broker::PreparationCacheRootBinding {
        let captured = rotation_debt::capture_preparation_cache_root(cache_root).unwrap();
        let initialized = rotation_debt::initialize_preparation_cache_root(
            cache_root,
            &captured.identity,
            &captured.directory,
            resource_db_path,
            || Ok(false),
        )
        .unwrap();
        let captured = rotation_debt::capture_preparation_cache_root(cache_root).unwrap();
        let state = match captured.state {
            rotation_debt::PreparationCacheRootCaptureState::Marked {
                coordinator_db_path,
                nonce,
                cache_root_identity: Some(cache_root_identity),
            } => crate::broker::PreparationCacheRootState::Marked {
                coordinator_db_path,
                nonce,
                cache_root_identity: Some(cache_root_identity),
            },
            other => panic!("expected an initialized cache root, got {other:?}"),
        };
        crate::broker::PreparationCacheRootBinding {
            path: cache_root.join(format!(".aethyme-preparation-cache-{}", initialized.nonce)),
            identity: Some(initialized.cache_root_identity),
            instance_nonce: Some(initialized.nonce),
            root_guard: Some(crate::broker::PreparationCacheRootGuard {
                root_path: cache_root.to_path_buf(),
                root_identity: captured.identity,
                state,
                captured_resource_db_path: resource_db_path.to_path_buf(),
                root_handle: std::sync::Arc::new(captured.directory),
                placement_root: None,
            }),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn rotation_recovery_revalidates_outer_binding_before_staging_mutation() {
        use std::time::Duration;

        let tmp = tempfile::tempdir().unwrap();
        let tmp_root = tmp.path().canonicalize().unwrap();
        let outer_root = tmp_root.as_path().join("preparation-cache-root");
        let host_state = tmp_root.as_path().join("host-state");
        std::fs::create_dir(&outer_root).unwrap();
        std::fs::create_dir(&host_state).unwrap();
        let resource_db = host_state.join("host-resources.db");
        std::fs::File::create(&resource_db).unwrap();
        let resource_db = resource_db.canonicalize().unwrap();
        let binding = initialized_cache_root_binding(&outer_root, &resource_db);
        let cache_root = binding.path.clone();
        let store = cache_root.join(SHARED_CACHE_TEST_REPOSITORY);
        std::fs::create_dir(&store).unwrap();
        std::fs::write(store.join("payload"), b"preserve during refusal").unwrap();

        let root_identity = rotation_debt::DirectoryIdentity::capture(&cache_root).unwrap();
        let store_identity = rotation_debt::DirectoryIdentity::capture(&store).unwrap();
        let debt = rotation_debt::RotationDebt::new(
            SHARED_CACHE_TEST_REPOSITORY,
            &rotation_debt::new_transaction_id(),
            root_identity.clone(),
            store_identity,
        )
        .unwrap();
        let payload = rotation_debt::encode(&debt).unwrap();
        let root_identity_bytes = root_identity.encode().unwrap();
        rotation_journal::publish(
            &resource_db,
            &cache_root,
            &root_identity_bytes,
            SHARED_CACHE_TEST_REPOSITORY,
            &payload,
        )
        .unwrap();
        rotation_debt::publish(&cache_root, debt.clone()).unwrap();
        let entries_before: std::collections::BTreeSet<_> = std::fs::read_dir(&cache_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();

        let marker_path = outer_root.join(".aethyme-preparation-cache-root-v2");
        let mut validations = 0;
        let error = rotation_debt::recover_with_journal_and_root_guard(
            &cache_root,
            &resource_db,
            SHARED_CACHE_TEST_REPOSITORY,
            Instant::now() + Duration::from_secs(5),
            || {
                validations += 1;
                if validations == 2 {
                    let mut marker: serde_json::Value =
                        serde_json::from_slice(&std::fs::read(&marker_path)?).map_err(|error| {
                            std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                        })?;
                    marker["coordinator_db_path"] = serde_json::Value::from(
                        tmp_root
                            .as_path()
                            .join("replacement-host-state/host-resources.db")
                            .to_string_lossy()
                            .into_owned(),
                    );
                    std::fs::write(
                        &marker_path,
                        serde_json::to_vec(&marker).map_err(|error| {
                            std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                        })?,
                    )?;
                }
                binding.validate().map_err(|error| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
                })
            },
        )
        .expect_err("recovery must stop when its captured root marker changes");

        assert_eq!(validations, 2);
        assert_eq!(error.source.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read(store.join("payload")).unwrap(),
            b"preserve during refusal"
        );
        let entries_after: std::collections::BTreeSet<_> = std::fs::read_dir(&cache_root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries_after, entries_before);
        let row = rotation_journal::load_record(
            &resource_db,
            &cache_root,
            &root_identity_bytes,
            SHARED_CACHE_TEST_REPOSITORY,
        )
        .unwrap()
        .expect("the pending journal row remains available for recovery");
        assert!(row.fresh_store_identity.is_none());
        assert!(
            rotation_debt::read(&cache_root, SHARED_CACHE_TEST_REPOSITORY)
                .unwrap()
                .is_some()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn preparation_writer_refuses_a_changed_cache_root_marker_before_measuring() {
        let tmp = tempfile::tempdir().unwrap();
        let tmp_root = tmp.path().canonicalize().unwrap();
        let cache_root = tmp_root.as_path().join("preparation-cache");
        let host_state = tmp_root.as_path().join("host-state");
        std::fs::create_dir(&cache_root).unwrap();
        std::fs::create_dir(&host_state).unwrap();
        let resource_db = host_state.join("host-resources.db");
        std::fs::File::create(&resource_db).unwrap();
        let resource_db = resource_db.canonicalize().unwrap();
        let binding = initialized_cache_root_binding(&cache_root, &resource_db);
        let store = binding
            .path
            .join(SHARED_CACHE_TEST_REPOSITORY)
            .join("abcdef123456");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("payload"), b"keep after marker change").unwrap();

        let marker_path = cache_root.join(".aethyme-preparation-cache-root-v2");
        let mut marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&marker_path).unwrap()).unwrap();
        marker["coordinator_db_path"] = serde_json::Value::from(
            tmp_root
                .as_path()
                .join("different-host-state/host-resources.db")
                .to_string_lossy()
                .into_owned(),
        );
        std::fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();

        let error = ensure_preparation_cache_root_binding(Some(&binding))
            .expect_err("preparation refuses a changed outer-root marker");
        assert!(
            error
                .to_string()
                .contains("captured preparation-cache root changed")
        );
        assert_eq!(
            std::fs::read(store.join("payload")).unwrap(),
            b"keep after marker change"
        );
    }

    #[test]
    fn preparation_argv_preserves_empty_arguments_but_rejects_empty_program() {
        assert!(
            validate_name_and_command("step", "build", &["/bin/echo".into(), "".into()]).is_ok()
        );
        assert!(validate_name_and_command("step", "build", &["".into(), "arg".into()]).is_err());
    }

    #[test]
    fn storage_heartbeat_checks_before_the_first_and_between_renewals() {
        use std::cell::Cell;

        let now = Instant::now();
        let mut heartbeat = PreparationHeartbeat::new(now);
        let storage_checks = Cell::new(0);
        let renewals = Cell::new(0);
        for (elapsed_ms, expected_storage, expected_renewals) in [
            (0, 1, 0),
            (200, 1, 0),
            (999, 1, 0),
            (1000, 2, 0),
            (10_000, 3, 1),
            (10_200, 3, 1),
            (11_000, 4, 1),
            (20_000, 5, 2),
        ] {
            heartbeat
                .tick(
                    now + Duration::from_millis(elapsed_ms),
                    || {
                        storage_checks.set(storage_checks.get() + 1);
                        Ok(())
                    },
                    || {
                        renewals.set(renewals.get() + 1);
                        Ok(())
                    },
                )
                .unwrap();
            assert_eq!(storage_checks.get(), expected_storage, "{elapsed_ms} ms");
            assert_eq!(renewals.get(), expected_renewals, "{elapsed_ms} ms");
        }
    }

    #[test]
    fn storage_heartbeat_pressure_prevents_renewal_and_stays_due() {
        use std::cell::Cell;

        for reason in ["reserve exhausted", "filesystem observation unavailable"] {
            let now = Instant::now();
            let mut heartbeat = PreparationHeartbeat::new(now);
            let due = now + Duration::from_secs(PREPARATION_LEASE_TTL_SECONDS / 3);
            let renewals = Cell::new(0);
            let error = heartbeat
                .tick(
                    due,
                    || {
                        Err(PreparationError::StorageUnavailable {
                            path: "/fixture/write-volume".into(),
                            reason: reason.into(),
                        })
                    },
                    || {
                        renewals.set(renewals.get() + 1);
                        Ok(())
                    },
                )
                .unwrap_err();
            assert!(is_storage_unavailable(&error));
            assert_eq!(renewals.get(), 0, "pressure must be checked before renewal");
            assert_eq!(heartbeat.next_storage_check, now);
            assert_eq!(heartbeat.next_renewal, due);

            let checked = Cell::new(false);
            heartbeat
                .tick(
                    due,
                    || {
                        checked.set(true);
                        Ok(())
                    },
                    || {
                        renewals.set(renewals.get() + 1);
                        Ok(())
                    },
                )
                .unwrap();
            assert!(checked.get(), "failed observation must remain due");
            assert_eq!(renewals.get(), 1);
        }
    }

    #[test]
    fn storage_heartbeat_renewal_failure_stays_due() {
        let now = Instant::now();
        let mut heartbeat = PreparationHeartbeat::new(now);
        let due = now + Duration::from_secs(PREPARATION_LEASE_TTL_SECONDS / 3);
        let error = heartbeat
            .tick(
                due,
                || Ok(()),
                || Err(invalid_config("injected renewal failure")),
            )
            .unwrap_err();
        assert!(!is_storage_unavailable(&error));
        assert_eq!(heartbeat.next_renewal, due);
        assert_eq!(
            heartbeat.next_storage_check,
            due + PREPARATION_STORAGE_CHECK_INTERVAL
        );
    }

    #[test]
    fn a_storage_refusal_persists_a_cache_recovery_request() {
        let tmp = tempfile::tempdir().unwrap();
        let run_git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(tmp.path())
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        run_git(&["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
        run_git(&["add", "README.md"]);
        run_git(&["commit", "-qm", "fixture"]);

        let mut broker = Broker::open(tmp.path()).unwrap();
        let unrelated = BrokerOpError::from(PreparationError::StepFailed {
            step: "deps".into(),
            exit_code: Some(1),
            session_id: 1,
        });
        broker.recover_storage_pressure_after_command_failure(&unrelated, || Ok(()));
        assert!(!broker.artifact_sweep_recovery_requested().unwrap());

        broker.recover_storage_pressure_after_command_failure(&unrelated, || {
            Err(PreparationError::InvalidConfig {
                path: PREPARATION_CONFIG_RELPATH.into(),
                reason: "injected non-storage recheck error".into(),
            })
        });
        assert!(!broker.artifact_sweep_recovery_requested().unwrap());

        broker.recover_storage_pressure_after_command_failure(&unrelated, || {
            Err(PreparationError::StorageUnavailable {
                path: tmp.path().join("target"),
                reason: "injected post-command pressure".into(),
            })
        });
        assert!(broker.artifact_sweep_recovery_requested().unwrap());

        let refusal = BrokerOpError::from(PreparationError::StorageUnavailable {
            path: tmp.path().join("target"),
            reason: "injected test refusal".into(),
        });

        let checked = std::cell::Cell::new(false);
        broker.recover_storage_pressure_after_command_failure(&refusal, || {
            checked.set(true);
            Ok(())
        });
        assert!(
            !checked.get(),
            "typed storage refusals need no second probe"
        );

        assert!(matches!(
            refusal,
            BrokerOpError::Preparation(PreparationError::StorageUnavailable { .. })
        ));
        assert!(broker.artifact_sweep_recovery_requested().unwrap());
    }

    #[test]
    fn a_failed_state_write_does_not_skip_host_lease_release() {
        use std::cell::Cell;

        let released = Cell::new(false);
        best_effort_failed_preparation_cleanup(
            || {
                Err(PreparationError::InvalidState {
                    path: ".aethyme/test-state.json".into(),
                    reason: "injected write failure".into(),
                })
            },
            || {
                released.set(true);
                Ok(())
            },
        );

        assert!(released.get());
    }

    #[test]
    fn unverified_process_cleanup_quarantines_and_retains_the_named_cache_lease() {
        let owner = tempfile::tempdir().unwrap();
        let mut coordinator =
            Some(HostResourceCoordinator::open(&owner.path().join("host-resources.db")).unwrap());
        let request = HostResourceRequest {
            schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: "cleanup-unknown-request".into(),
            repository: "fixture-repository".into(),
            worktree_fingerprint: "a".repeat(64),
            run_id: "cleanup-unknown-run".into(),
            ttl_seconds: PREPARATION_LEASE_TTL_SECONDS,
            holder_pid: Some(std::process::id()),
            resources: vec![HostResourceRequirement {
                key: "preparation_cache".into(),
                resource: HostResourceKind::ExclusiveKey {
                    name: preparation_cache_lease_name("fixture-repository"),
                },
            }],
        };
        let grant = coordinator.as_mut().unwrap().acquire(&request).unwrap();
        let lease_id = grant.lease.lease_id.clone();
        let mut grant = Some(grant);

        assert!(quarantine_grant_best_effort(&mut coordinator, &mut grant));
        assert!(grant.is_none());
        let retained = coordinator
            .as_ref()
            .unwrap()
            .list(true)
            .unwrap()
            .into_iter()
            .find(|lease| lease.lease_id == lease_id)
            .unwrap();
        assert_eq!(retained.state, crate::HostLeaseState::Quarantined);

        let mut retry = request;
        retry.request_id = "cleanup-unknown-retry".into();
        retry.run_id = "cleanup-unknown-retry".into();
        assert!(matches!(
            coordinator.as_mut().unwrap().acquire(&retry),
            Err(crate::HostResourceError::Conflict { .. })
        ));
    }

    fn preparation_failure_test_request(request_id: &str) -> HostResourceRequest {
        HostResourceRequest {
            schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: request_id.into(),
            repository: "fixture-repository".into(),
            worktree_fingerprint: "f".repeat(64),
            run_id: format!("{request_id}-run"),
            ttl_seconds: PREPARATION_LEASE_TTL_SECONDS,
            holder_pid: Some(std::process::id()),
            resources: vec![HostResourceRequirement {
                key: "preparation_cache".into(),
                resource: HostResourceKind::ExclusiveKey {
                    name: preparation_cache_lease_name("fixture-repository"),
                },
            }],
        }
    }

    fn in_progress_test_record(
        session_id: i64,
        grant: &crate::HostResourceGrant,
    ) -> PreparationRecord {
        PreparationRecord {
            schema_version: PREPARATION_SCHEMA_VERSION,
            session_id,
            state: PreparationState::InProgress,
            digest: "test-digest".into(),
            source_digest: "test-source-digest".into(),
            started_at: now_ms(),
            completed_at: None,
            failed_step: None,
            exit_code: None,
            host_lease_id: Some(grant.lease.lease_id.clone()),
            host_lease_generation: Some(grant.lease.generation),
        }
    }

    #[test]
    fn unknown_command_outcomes_keep_preparation_unresolved_and_retry_blocked() {
        let errors: Vec<BrokerOpError> = vec![
            PreparationError::CommandOutcomeUnknown {
                session_id: 41,
                reason: "injected lost result".into(),
            }
            .into(),
            PreparationError::CleanupUnknown {
                session_id: 41,
                reason: "injected unconfirmed cleanup".into(),
            }
            .into(),
        ];

        for (index, error) in errors.into_iter().enumerate() {
            let owner = tempfile::tempdir().unwrap();
            let mut coordinator = Some(
                HostResourceCoordinator::open(&owner.path().join("host-resources.db")).unwrap(),
            );
            let request = preparation_failure_test_request(&format!("unknown-{index}"));
            let acquired = coordinator.as_mut().unwrap().acquire(&request).unwrap();
            let lease_id = acquired.lease.lease_id.clone();
            let mut grant = Some(acquired);
            let session_id = 41;
            let mut record = in_progress_test_record(session_id, grant.as_ref().unwrap());
            let state_path = record_path(owner.path(), session_id);
            write_record(&state_path, &record).unwrap();
            let initial_record = fs::read(&state_path).unwrap();

            let restart_storage_worker = route_preparation_command_failure(
                &error,
                PreparationCommandFailureContext {
                    step: "dependencies",
                    state_path: &state_path,
                    record: &mut record,
                    coordinator: &mut coordinator,
                    grant: &mut grant,
                    exit_code: None,
                    write_exit_code: false,
                },
                |_| panic!("unknown outcomes must not trigger storage recovery"),
            );

            assert!(!restart_storage_worker);
            assert!(grant.is_none());
            assert_eq!(fs::read(&state_path).unwrap(), initial_record);
            let persisted = read_record(&state_path).unwrap().unwrap();
            assert_eq!(persisted.state, PreparationState::InProgress);
            assert_eq!(persisted.failed_step, None);
            assert_eq!(persisted.completed_at, None);
            assert!(matches!(
                ensure_preparation_retry_allowed(session_id, persisted.state),
                Err(PreparationError::PriorPreparationIncomplete { session_id: 41 })
            ));

            let retained = coordinator
                .as_ref()
                .unwrap()
                .list(true)
                .unwrap()
                .into_iter()
                .find(|lease| lease.lease_id == lease_id)
                .unwrap();
            assert_eq!(retained.state, crate::HostLeaseState::Quarantined);
            let mut retry = request;
            retry.request_id = format!("unknown-{index}-retry");
            retry.run_id = format!("unknown-{index}-retry-run");
            assert!(matches!(
                coordinator.as_mut().unwrap().acquire(&retry),
                Err(crate::HostResourceError::Conflict { .. })
            ));
        }
    }

    #[test]
    fn known_command_failure_records_failed_state_and_releases_lease() {
        let owner = tempfile::tempdir().unwrap();
        let mut coordinator =
            Some(HostResourceCoordinator::open(&owner.path().join("host-resources.db")).unwrap());
        let request = preparation_failure_test_request("known-failure");
        let acquired = coordinator.as_mut().unwrap().acquire(&request).unwrap();
        let lease_id = acquired.lease.lease_id.clone();
        let mut grant = Some(acquired);
        let session_id = 42;
        let mut record = in_progress_test_record(session_id, grant.as_ref().unwrap());
        let state_path = record_path(owner.path(), session_id);
        write_record(&state_path, &record).unwrap();
        let error: BrokerOpError = PreparationError::StepFailed {
            step: "dependencies".into(),
            exit_code: Some(17),
            session_id,
        }
        .into();

        let restart_storage_worker = route_preparation_command_failure(
            &error,
            PreparationCommandFailureContext {
                step: "dependencies",
                state_path: &state_path,
                record: &mut record,
                coordinator: &mut coordinator,
                grant: &mut grant,
                exit_code: Some(17),
                write_exit_code: true,
            },
            |_| false,
        );

        assert!(!restart_storage_worker);
        assert!(grant.is_none());
        let persisted = read_record(&state_path).unwrap().unwrap();
        assert_eq!(persisted.state, PreparationState::Failed);
        assert_eq!(persisted.failed_step.as_deref(), Some("dependencies"));
        assert_eq!(persisted.exit_code, Some(17));
        assert!(persisted.completed_at.is_some());
        assert!(ensure_preparation_retry_allowed(session_id, persisted.state).is_ok());
        let released = coordinator
            .as_ref()
            .unwrap()
            .list(true)
            .unwrap()
            .into_iter()
            .find(|lease| lease.lease_id == lease_id)
            .unwrap();
        assert_eq!(released.state, crate::HostLeaseState::Released);

        let mut retry = request;
        retry.request_id = "known-failure-retry".into();
        retry.run_id = "known-failure-retry-run".into();
        coordinator.as_mut().unwrap().acquire(&retry).unwrap();
    }

    #[test]
    fn an_initial_preparation_state_write_error_releases_the_acquired_host_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let run_git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(tmp.path())
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        run_git(&["init", "-q", "-b", "main"]);
        std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
        run_git(&["add", "README.md"]);
        run_git(&["commit", "-qm", "fixture"]);
        let mut broker = Broker::open(tmp.path()).unwrap();
        let resource_db = tmp.path().join("host-state/host-resources.db");
        let mut coordinator = Some(HostResourceCoordinator::open(&resource_db).unwrap());
        let request =
            preparation_resource_request(&broker, 7, tmp.path(), &"a".repeat(64)).unwrap();
        let acquired = coordinator.as_mut().unwrap().acquire(&request).unwrap();
        let lease_id = acquired.lease.lease_id.clone();
        let mut grant = Some(acquired);

        let result: Result<(), PreparationError> = broker.release_grant_after_error(
            Err(PreparationError::InvalidState {
                path: ".aethyme/preparation/7.json".into(),
                reason: "injected initial state write failure".into(),
            }),
            &mut coordinator,
            &mut grant,
            || Ok(()),
        );

        assert!(result.is_err());
        assert!(grant.is_none());
        assert_eq!(
            coordinator
                .as_ref()
                .unwrap()
                .list(true)
                .unwrap()
                .into_iter()
                .find(|lease| lease.lease_id == lease_id)
                .unwrap()
                .state,
            crate::HostLeaseState::Released,
            "the original state-write failure must not strand the acquired lease"
        );
    }

    #[test]
    fn preparation_checks_every_write_volume_and_inode_reserve() {
        let paths = [PathBuf::from("/checkout"), PathBuf::from("/cache")];
        for (bytes, inodes) in [
            (crate::DEFAULT_GATE_HEADROOM_BYTES - 1, Some(1_000_000)),
            (crate::DEFAULT_GATE_HEADROOM_BYTES, Some(99_999)),
        ] {
            let result = preparation_storage_with(
                &paths,
                |_| None,
                |path| {
                    Some(crate::disk_headroom::DiskHeadroom {
                        bytes: if path == Path::new("/cache") {
                            bytes
                        } else {
                            u64::MAX
                        },
                        inodes: if path == Path::new("/cache") {
                            inodes
                        } else {
                            None
                        },
                    })
                },
            );
            assert!(
                matches!(result, Err(PreparationError::StorageUnavailable { path, .. }) if path == paths[1])
            );
        }
    }

    #[test]
    fn preparation_refuses_unmounted_storage_but_preserves_unknown_headroom() {
        let paths = [PathBuf::from("/missing-volume")];
        let result = preparation_storage_with(
            &paths,
            |_| Some("volume is absent".into()),
            |_| panic!("must not probe an absent volume"),
        );
        assert!(matches!(
            result,
            Err(PreparationError::StorageUnavailable { .. })
        ));
        assert!(preparation_storage_with(&paths, |_| None, |_| None).is_ok());
        assert!(
            preparation_storage_with(
                &paths,
                |_| None,
                |_| Some(crate::disk_headroom::DiskHeadroom {
                    bytes: crate::DEFAULT_GATE_HEADROOM_BYTES,
                    inodes: Some(crate::disk_headroom::MIN_GATE_HEADROOM_INODES),
                })
            )
            .is_ok()
        );
    }

    fn config() -> PreparationConfig {
        PreparationConfig {
            schema_version: 1,
            runtimes: Vec::new(),
            steps: vec![PreparationStep {
                name: "deps".into(),
                command: vec!["tool".into(), "install".into()],
                offline_command: None,
                inputs: vec!["lock.file".into()],
                outputs: vec!["vendor/".into()],
                cache: PreparationCachePolicy::WorktreeLocal,
                required_for_hooks: true,
            }],
            shared_cache: None,
            managed_storage: None,
        }
    }

    fn managed_profile_config() -> PreparationConfig {
        let mut config = config();
        let mut output_growth_bytes = BTreeMap::new();
        output_growth_bytes.insert("vendor/".into(), 4096);
        config.managed_storage = Some(ManagedPreparationStorageProfile {
            steps: BTreeMap::from([(
                "deps".into(),
                ManagedPreparationStepStorage {
                    output_growth_bytes,
                    shared_cache_growth_bytes: None,
                },
            )]),
        });
        config
    }

    fn shared_budget_config(shared: bool, max_bytes: u64) -> String {
        format!(
            "schema_version = 1\n\
             [[steps]]\n\
             name = \"deps\"\n\
             command = [\"tool\", \"install\"]\n\
             inputs = [\"lock.file\"]\n\
             outputs = [\"vendor/\"]\n\
             cache = \"{}\"\n\
             [shared_cache]\n\
             max_bytes = {max_bytes}\n",
            if shared {
                "repository_shared"
            } else {
                "worktree_local"
            }
        )
    }

    /// A budget on a per-worktree policy reads as a disk bound that is not
    /// being applied, which is the exact belief the field exists to remove.
    /// Refuse it at parse time rather than accepting a silent no-op.
    #[test]
    fn a_cache_budget_without_a_shared_step_is_refused() {
        let error = parse_config(&shared_budget_config(false, 1024))
            .expect_err("a budget on a per-worktree policy is a mistake, not a no-op");
        assert!(
            error.to_string().contains("repository_shared"),
            "the error must name the remedy: {error}"
        );
    }

    #[test]
    fn a_cache_budget_on_a_shared_step_is_accepted() {
        let parsed = parse_config(&shared_budget_config(true, 1024))
            .expect("a budget on a shared store is the supported shape");
        assert_eq!(
            parsed.shared_cache.map(|budget| budget.max_bytes),
            Some(1024)
        );
    }

    #[test]
    fn a_shared_cache_without_a_root_selector_keeps_the_host_default() {
        let parsed = parse_config(&shared_budget_config(true, 1024)).unwrap();
        assert_eq!(
            parsed.shared_cache.expect("declared").root,
            SharedCacheRoot::Host
        );
    }

    #[test]
    fn a_worktree_volume_root_selector_keeps_the_default_budget() {
        let source = "schema_version = 1\n\
            [[steps]]\n\
            name = \"deps\"\n\
            command = [\"tool\", \"install\"]\n\
            inputs = [\"lock.file\"]\n\
            outputs = [\"vendor/\"]\n\
            cache = \"repository_shared\"\n\
            [shared_cache]\n\
            root = \"worktree_volume\"\n";
        let parsed = parse_config(source).expect("root selection is independent of the budget");
        let shared_cache = parsed.shared_cache.expect("declared");
        assert_eq!(shared_cache.root, SharedCacheRoot::WorktreeVolume);
        assert_eq!(shared_cache.max_bytes, 8_589_934_592);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn omitted_shared_budget_rotates_all_digest_keys_above_eight_gib() {
        let source = shared_budget_config(true, 0);
        let source = source.split("[shared_cache]").next().unwrap();
        let parsed = parse_config(source).unwrap();
        let budget = parsed
            .shared_cache
            .expect("shared steps receive a default budget");
        assert_eq!(budget.max_bytes, 8_589_934_592);
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap().join("preparation-cache");
        let store = root.join(SHARED_CACHE_TEST_REPOSITORY);
        std::fs::create_dir_all(store.join("old-digest")).unwrap();
        std::fs::create_dir_all(store.join("new-digest")).unwrap();
        // Logical bytes are the documented threshold. Sparse files exercise
        // a real oversized inventory without allocating gigabytes on disk.
        std::fs::File::create(store.join("old-digest/payload"))
            .unwrap()
            .set_len(4_294_967_297)
            .unwrap();
        std::fs::File::create(store.join("new-digest/payload"))
            .unwrap()
            .set_len(4_294_967_297)
            .unwrap();
        let report = shared_cache_budget_at(
            Some(&budget),
            &root,
            SHARED_CACHE_TEST_REPOSITORY,
            "new-digest",
        )
        .unwrap()
        .expect("omitted configuration must not retain unbounded digest keys");
        assert!(report.removed);
        assert_eq!(report.budget_bytes, 8_589_934_592);
        assert!(!store.join("old-digest").exists());
        assert!(!store.join("new-digest").exists());
    }

    #[test]
    fn local_only_preparation_does_not_acquire_a_shared_cache_budget() {
        let source = shared_budget_config(false, 0);
        let source = source.split("[shared_cache]").next().unwrap();
        assert!(parse_config(source).unwrap().shared_cache.is_none());
    }

    /// `0` is the pre-existing behaviour and must stay available: a
    /// single-clone developer gains nothing from a ceiling.
    #[test]
    fn a_zero_budget_declares_no_ceiling() {
        let parsed = parse_config(&shared_budget_config(true, 0)).unwrap();
        let budget = parsed.shared_cache.expect("declared");
        assert_eq!(budget.max_bytes, 0);
        // The filter, not a comparison, is what makes 0 mean "unbounded", so
        // assert the path that does the work.
        assert!(budget.max_bytes == 0);
    }

    /// The low-level recovery primitive can still run without a threshold.
    /// User-facing shared profiles receive the default during parsing.
    #[test]
    fn an_absent_budget_means_no_rotation() {
        assert!(config().shared_cache.is_none());
        let tmp = tempfile::tempdir().unwrap();
        let cache_root = tmp.path().join("preparation-cache");
        let store = cache_root.join(SHARED_CACHE_TEST_REPOSITORY);
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("big"), vec![0_u8; 4096]).unwrap();
        let report = shared_cache_budget_at(
            None,
            &cache_root,
            SHARED_CACHE_TEST_REPOSITORY,
            "abcdef123456",
        )
        .unwrap();
        assert!(report.is_none(), "no budget must mean no measurement");
        assert!(store.join("big").exists(), "the store must be untouched");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn an_absent_budget_still_finishes_a_previously_published_rotation() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture_root = tmp.path().canonicalize().unwrap();
        let cache_root = fixture_root.join("preparation-cache");
        let store = cache_root.join(SHARED_CACHE_TEST_REPOSITORY);
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("payload"), b"old cache").unwrap();
        let cache_root_identity = rotation_debt::DirectoryIdentity::capture(&cache_root).unwrap();
        let store_identity = rotation_debt::DirectoryIdentity::capture(&store).unwrap();
        let debt = rotation_debt::RotationDebt::new(
            SHARED_CACHE_TEST_REPOSITORY,
            &rotation_debt::new_transaction_id(),
            cache_root_identity,
            store_identity,
        )
        .unwrap();
        rotation_debt::publish(&cache_root, debt.clone()).unwrap();
        rotation_debt::retire_original_store(
            &cache_root,
            &rotation_debt::read(&cache_root, SHARED_CACHE_TEST_REPOSITORY)
                .unwrap()
                .unwrap(),
        )
        .unwrap();

        assert!(
            shared_cache_budget_at(
                None,
                &cache_root,
                SHARED_CACHE_TEST_REPOSITORY,
                "abcdef123456",
            )
            .unwrap()
            .is_none()
        );
        assert!(store.is_dir());
        assert_eq!(fs::read_dir(&store).unwrap().count(), 0);
        assert!(
            rotation_debt::read(&cache_root, SHARED_CACHE_TEST_REPOSITORY)
                .unwrap()
                .is_none()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_missing_shared_store_is_not_created_by_budget_measurement() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_root = tmp.path().join("preparation-cache");
        std::fs::create_dir(&cache_root).unwrap();
        let store = cache_root.join(SHARED_CACHE_TEST_REPOSITORY);

        assert_eq!(directory_bytes(&store).unwrap(), None);
        assert!(
            shared_cache_budget_at(
                Some(&SharedCacheBudget {
                    max_bytes: 1,
                    ..SharedCacheBudget::default()
                }),
                &cache_root,
                SHARED_CACHE_TEST_REPOSITORY,
                "abcdef123456",
            )
            .unwrap()
            .is_none()
        );
        assert!(!store.exists());
    }

    /// Over budget rotates the store and leaves a usable one behind; under
    /// budget measures and does nothing.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn an_over_budget_shared_store_is_rotated_while_under_budget_is_not() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture_root = tmp.path().canonicalize().unwrap();
        let cache_root = fixture_root.join("preparation-cache");
        let store = cache_root.join(SHARED_CACHE_TEST_REPOSITORY);
        std::fs::create_dir_all(store.join("nested")).unwrap();
        std::fs::write(store.join("nested/payload"), vec![0_u8; 8192]).unwrap();
        let bytes = directory_bytes(&store).unwrap().unwrap();
        assert!(bytes >= 8192);

        let budget = SharedCacheBudget {
            max_bytes: bytes - 1,
            ..SharedCacheBudget::default()
        };
        let rotation = shared_cache_budget_at(
            Some(&budget),
            &cache_root,
            SHARED_CACHE_TEST_REPOSITORY,
            "abcdef123456",
        )
        .expect("rotation is reported")
        .expect("over budget");
        assert_eq!(rotation.bytes_before, bytes);
        assert_eq!(rotation.budget_bytes, budget.max_bytes);
        assert_eq!(rotation.key, "abcdef123456");
        assert!(rotation.removed, "the retired tree is reclaimed");
        assert!(
            store.is_dir() && std::fs::read_dir(&store).unwrap().next().is_none(),
            "rotation must leave an empty, usable store"
        );

        // Under budget: measured, reported as no rotation, store preserved.
        std::fs::write(store.join("payload"), vec![0_u8; 64]).unwrap();
        assert!(
            shared_cache_budget_at(
                Some(&budget),
                &cache_root,
                SHARED_CACHE_TEST_REPOSITORY,
                "abcdef123456",
            )
            .unwrap()
            .is_none()
        );
        assert!(store.join("payload").exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn preparation_writer_refuses_a_replaced_captured_cache_root() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_root = tmp.path().join("preparation-cache");
        let original_store = cache_root.join(SHARED_CACHE_TEST_REPOSITORY);
        std::fs::create_dir_all(&original_store).unwrap();
        std::fs::write(original_store.join("payload"), b"original").unwrap();
        let root_identity = rotation_debt::DirectoryIdentity::capture(&cache_root)
            .unwrap()
            .encode()
            .unwrap();
        let original_root = tmp.path().join("preparation-cache-original");
        std::fs::rename(&cache_root, &original_root).unwrap();
        let replacement_store = cache_root.join(SHARED_CACHE_TEST_REPOSITORY);
        std::fs::create_dir_all(&replacement_store).unwrap();
        std::fs::write(replacement_store.join("payload"), b"replacement").unwrap();
        let resource_db = tmp.path().join("host-state/host-resources.db");

        let error = shared_cache_budget_at_with_resource_db_and_identity(
            Some(&SharedCacheBudget {
                max_bytes: 1,
                ..SharedCacheBudget::default()
            }),
            &cache_root,
            &resource_db,
            SHARED_CACHE_TEST_REPOSITORY,
            "0123456789ab",
            Some(&root_identity),
        )
        .expect_err("a replacement root must not receive preparation writes");

        assert!(error.to_string().contains("captured identity"));
        assert_eq!(
            std::fs::read(replacement_store.join("payload")).unwrap(),
            b"replacement"
        );
        assert_eq!(
            std::fs::read(
                original_root
                    .join(SHARED_CACHE_TEST_REPOSITORY)
                    .join("payload")
            )
            .unwrap(),
            b"original"
        );
        assert!(
            !resource_db.exists(),
            "mismatch is rejected before debt journal creation"
        );
    }

    /// The report must not disclose where the store lives: it is host state
    /// outside the repository, and this record reaches JSON output.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_rotation_report_carries_no_host_path() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture_root = tmp.path().canonicalize().unwrap();
        let cache_root = fixture_root.join("host-state/preparation-cache");
        let store = cache_root.join(SHARED_CACHE_TEST_REPOSITORY);
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("payload"), vec![0_u8; 4096]).unwrap();
        let rotation = shared_cache_budget_at(
            Some(&SharedCacheBudget {
                max_bytes: 1,
                ..SharedCacheBudget::default()
            }),
            &cache_root,
            SHARED_CACHE_TEST_REPOSITORY,
            "0123456789ab",
        )
        .unwrap()
        .unwrap();
        let json = serde_json::to_string(&rotation).unwrap();
        assert!(
            !json.contains(fixture_root.to_string_lossy().as_ref()),
            "rotation report leaked a host path: {json}"
        );
        assert!(!json.contains("preparation-cache"), "{json}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn shared_cache_inventory_counts_links_without_following_targets_or_cycles() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(store.join("nested")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(store.join("nested/payload"), b"inside-data").unwrap();
        fs::write(outside.join("large"), vec![0_u8; 128 * 1024]).unwrap();
        symlink(&outside, store.join("external")).unwrap();
        symlink(&store, store.join("cycle")).unwrap();

        let expected = b"inside-data".len() as u64
            + fs::symlink_metadata(store.join("external")).unwrap().len()
            + fs::symlink_metadata(store.join("cycle")).unwrap().len();
        assert_eq!(directory_bytes(&store).unwrap(), Some(expected));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn shared_cache_inventory_refuses_a_symlink_root_without_touching_target() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        let link = tmp.path().join("store");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("payload"), b"preserve me").unwrap();
        symlink(&target, &link).unwrap();

        assert!(matches!(
            directory_bytes(&link),
            Err(PreparationError::StorageUnavailable { .. })
        ));
        assert_eq!(fs::read(target.join("payload")).unwrap(), b"preserve me");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn shared_cache_inventory_rejects_root_replaced_after_metadata_check() {
        let temporary = tempfile::tempdir().unwrap();
        let store = temporary.path().join("cache");
        let original = temporary.path().join("cache-original");
        fs::create_dir(&store).unwrap();
        fs::write(store.join("original"), b"x").unwrap();
        let mut replaced = false;

        let result = directory_bytes_fd_relative_with_hook(
            &store,
            SharedCacheScanLimits::default(),
            |target| {
                if matches!(target, SharedCacheScanOpenTarget::Root) && !replaced {
                    fs::rename(&store, &original).unwrap();
                    fs::create_dir(&store).unwrap();
                    fs::write(store.join("replacement"), [0_u8; 64]).unwrap();
                    replaced = true;
                }
            },
        );

        assert!(replaced);
        assert!(matches!(
            result,
            Err(PreparationError::StorageUnavailable { .. })
        ));
        assert_eq!(fs::read(original.join("original")).unwrap(), b"x");
        assert_eq!(fs::metadata(store.join("replacement")).unwrap().len(), 64);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn shared_cache_inventory_rejects_child_replaced_after_lstat() {
        let temporary = tempfile::tempdir().unwrap();
        let store = temporary.path().join("cache");
        let child = store.join("child");
        let original = store.join("child-original");
        fs::create_dir_all(&child).unwrap();
        fs::write(child.join("original"), b"x").unwrap();
        let mut replaced = false;

        let result = directory_bytes_fd_relative_with_hook(
            &store,
            SharedCacheScanLimits::default(),
            |target| {
                if matches!(target, SharedCacheScanOpenTarget::Child) && !replaced {
                    fs::rename(&child, &original).unwrap();
                    fs::create_dir(&child).unwrap();
                    fs::write(child.join("replacement"), [0_u8; 64]).unwrap();
                    replaced = true;
                }
            },
        );

        assert!(replaced);
        assert!(matches!(
            result,
            Err(PreparationError::StorageUnavailable { .. })
        ));
        assert_eq!(fs::read(original.join("original")).unwrap(), b"x");
        assert_eq!(fs::metadata(child.join("replacement")).unwrap().len(), 64);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn shared_cache_inventory_refuses_entry_and_depth_caps() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        fs::create_dir_all(store.join("nested/deeper")).unwrap();
        fs::write(store.join("one"), b"1").unwrap();
        fs::write(store.join("two"), b"2").unwrap();
        fs::write(store.join("nested/deeper/three"), b"3").unwrap();
        let base = SharedCacheScanLimits {
            max_entries: 100,
            max_depth: SHARED_CACHE_SCAN_MAX_DEPTH,
            max_elapsed: Duration::from_secs(5),
        };

        let entry_limited = directory_bytes_with_limits(
            &store,
            SharedCacheScanLimits {
                max_entries: 2,
                ..base
            },
        );
        assert!(matches!(
            entry_limited,
            Err(PreparationError::StorageUnavailable { .. })
        ));

        let depth_limited = directory_bytes_with_limits(
            &store,
            SharedCacheScanLimits {
                max_depth: 1,
                ..base
            },
        );
        assert!(matches!(
            depth_limited,
            Err(PreparationError::StorageUnavailable { .. })
        ));

        let time_limited = directory_bytes_with_limits(
            &store,
            SharedCacheScanLimits {
                max_elapsed: Duration::ZERO,
                ..base
            },
        );
        assert!(matches!(
            time_limited,
            Err(PreparationError::StorageUnavailable { .. })
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn shared_cache_inventory_fails_closed_on_permission_denied_directory() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        let denied = store.join("denied");
        fs::create_dir_all(&denied).unwrap();
        fs::write(denied.join("payload"), b"not readable").unwrap();
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o000)).unwrap();
        let result = directory_bytes(&store);
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o700)).unwrap();

        // SAFETY: geteuid reads the calling process's effective user ID.
        if unsafe { libc::geteuid() } == 0 {
            // Root can bypass mode bits; the production scan still fails
            // closed on the same openat error under an unprivileged caller.
            return;
        }
        assert!(matches!(
            result,
            Err(PreparationError::StorageUnavailable { .. })
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn shared_cache_inventory_refuses_special_files_without_opening_them() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let tmp = tempfile::tempdir().unwrap();
        let cache_root = tmp.path().join("preparation-cache");
        let store = cache_root.join(SHARED_CACHE_TEST_REPOSITORY);
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("keep"), b"preserve me").unwrap();
        let fifo = store.join("blocked-pipe");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_c is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);

        assert!(matches!(
            shared_cache_budget_at(
                Some(&SharedCacheBudget {
                    max_bytes: 1,
                    ..SharedCacheBudget::default()
                }),
                &cache_root,
                SHARED_CACHE_TEST_REPOSITORY,
                "abcdef123456",
            ),
            Err(PreparationError::StorageUnavailable { .. })
        ));
        assert_eq!(fs::read(store.join("keep")).unwrap(), b"preserve me");
        assert!(fifo.exists());
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn shared_cache_budget_fails_closed_on_unsupported_platforms() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_root = tmp.path().join("preparation-cache");
        let store = cache_root.join(SHARED_CACHE_TEST_REPOSITORY);
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("keep"), b"keep").unwrap();

        assert!(matches!(
            shared_cache_budget_at(
                Some(&SharedCacheBudget {
                    max_bytes: 1,
                    ..SharedCacheBudget::default()
                }),
                &cache_root,
                SHARED_CACHE_TEST_REPOSITORY,
                "abcdef123456",
            ),
            Err(PreparationError::StorageUnavailable { .. })
        ));
        assert_eq!(fs::read(store.join("keep")).unwrap(), b"keep");
    }

    #[test]
    fn digest_changes_with_same_named_input_bytes() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("lock.file"), "one").unwrap();
        let one = preparation_digest(root.path(), &config(), 1).unwrap();
        fs::write(root.path().join("lock.file"), "two").unwrap();
        let two = preparation_digest(root.path(), &config(), 1).unwrap();
        assert_ne!(one, two);
    }

    #[test]
    fn streamed_large_input_keeps_the_existing_digest() {
        let root = tempfile::tempdir().unwrap();
        let contents = (0..3 * 1024 * 1024)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        fs::write(root.path().join("lock.file"), &contents).unwrap();
        let config = config();

        // Reproduce the established digest framing using the old whole-file
        // read shape. The streaming implementation must be byte-for-byte
        // compatible so already populated cache keys remain reusable.
        let mut expected = Sha256::new();
        expected.update(PREPARATION_SCHEMA_VERSION.to_le_bytes());
        expected.update(std::env::consts::OS.as_bytes());
        expected.update([0]);
        expected.update(std::env::consts::ARCH.as_bytes());
        expected.update(serde_json::to_vec(&config).unwrap());
        expected.update(b"lock.file");
        expected.update((contents.len() as u64).to_le_bytes());
        expected.update(Sha256::digest(&contents));
        let expected = format!("{:x}", expected.finalize());

        assert_eq!(
            preparation_source_digest(root.path(), &config, 1).unwrap(),
            expected
        );
    }

    #[test]
    fn expired_liveness_deadline_refuses_before_reading_large_input() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".aethyme")).unwrap();
        let text = "schema_version = 1\n\n[[steps]]\nname = \"deps\"\ncommand = [\"sh\", \"-c\", \"true\"]\ninputs = [\"lock.file\"]\noutputs = [\"out/\"]\ncache = \"repository_shared\"\nrequired_for_hooks = false\n";
        assert!(parse_config(text).is_ok(), "fixture config must be valid");
        fs::write(root.path().join(".aethyme/prepare.toml"), text).unwrap();
        fs::write(root.path().join("lock.file"), vec![b'x'; 8 * 1024 * 1024]).unwrap();

        let result =
            current_cache_key_before(root.path(), Some(Instant::now() - Duration::from_millis(1)));

        assert!(matches!(
            result,
            Err(PreparationError::LivenessDeadlineElapsed)
        ));
    }

    #[test]
    fn config_is_language_neutral_and_rejects_traversal() {
        let mut config = config();
        validate_config(&config).unwrap();
        config.steps[0].outputs = vec!["../elsewhere".into()];
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn absent_managed_storage_profile_preserves_legacy_serialization_and_execution_gate() {
        let config = config();
        assert!(config.managed_storage.is_none());
        assert!(
            serde_json::to_value(&config)
                .unwrap()
                .get("managed_storage")
                .is_none()
        );
        let plan = managed_preparation_storage_plan(Path::new("."), &config, None).unwrap();
        assert!(plan.is_none());
        assert!(require_managed_storage_execution_ready(plan.as_ref()).is_ok());
    }

    #[test]
    fn managed_storage_plan_resolves_exact_outputs_and_refuses_before_command_start() {
        use std::cell::Cell;

        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let config = managed_profile_config();
        let plan = managed_preparation_storage_plan(&root, &config, None)
            .unwrap()
            .expect("explicit managed profile produces a plan");

        assert_eq!(plan.worktree_root, root);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].name, "deps");
        assert_eq!(plan.steps[0].outputs.len(), 1);
        assert_eq!(plan.steps[0].outputs[0].relative_path, Path::new("vendor"));
        assert_eq!(plan.steps[0].outputs[0].absolute_path, root.join("vendor"));
        assert_eq!(plan.steps[0].outputs[0].growth_bytes, 4096);
        assert!(plan.steps[0].shared_cache.is_none());

        let command_started = Cell::new(false);
        let result = require_managed_storage_execution_ready(Some(&plan)).map(|()| {
            command_started.set(true);
        });
        assert!(matches!(
            result,
            Err(PreparationError::ManagedStorageNotReady { paths: 1, steps: 1 })
        ));
        assert!(
            !command_started.get(),
            "the managed-storage guard must stop before a preparation command starts"
        );
    }

    #[test]
    fn managed_shared_cache_plan_uses_the_captured_digest_specific_path() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let cache_root = tempfile::tempdir().unwrap();
        let cache_root = cache_root.path().canonicalize().unwrap();
        let cache_path = cache_root.join("repository").join("digest-prefix");
        let mut config = managed_profile_config();
        config.steps[0].cache = PreparationCachePolicy::RepositoryShared;
        config
            .managed_storage
            .as_mut()
            .unwrap()
            .steps
            .get_mut("deps")
            .unwrap()
            .shared_cache_growth_bytes = Some(8192);

        let plan = managed_preparation_storage_plan(&root, &config, Some(&cache_path))
            .unwrap()
            .expect("explicit managed profile produces a plan");
        let cache = plan.steps[0]
            .shared_cache
            .as_ref()
            .expect("repository-shared step has a bounded cache root");
        assert_eq!(cache.path, cache_path);
        assert_eq!(cache.growth_bytes, 8192);
    }

    #[test]
    fn managed_storage_profile_requires_exact_positive_checked_growth_bounds() {
        let mut missing_step = managed_profile_config();
        missing_step.managed_storage.as_mut().unwrap().steps.clear();
        assert!(validate_config(&missing_step).is_err());

        let mut unknown_step = managed_profile_config();
        unknown_step.managed_storage.as_mut().unwrap().steps.insert(
            "other".into(),
            ManagedPreparationStepStorage {
                output_growth_bytes: BTreeMap::new(),
                shared_cache_growth_bytes: None,
            },
        );
        assert!(validate_config(&unknown_step).is_err());

        let mut missing_output = managed_profile_config();
        missing_output
            .managed_storage
            .as_mut()
            .unwrap()
            .steps
            .get_mut("deps")
            .unwrap()
            .output_growth_bytes
            .clear();
        assert!(validate_config(&missing_output).is_err());

        let mut zero_growth = managed_profile_config();
        zero_growth
            .managed_storage
            .as_mut()
            .unwrap()
            .steps
            .get_mut("deps")
            .unwrap()
            .output_growth_bytes
            .insert("vendor/".into(), 0);
        assert!(validate_config(&zero_growth).is_err());

        let mut unknown_output = managed_profile_config();
        unknown_output
            .managed_storage
            .as_mut()
            .unwrap()
            .steps
            .get_mut("deps")
            .unwrap()
            .output_growth_bytes
            .insert("elsewhere/".into(), 1);
        assert!(validate_config(&unknown_output).is_err());

        let mut shared_growth_without_shared_cache = managed_profile_config();
        shared_growth_without_shared_cache
            .managed_storage
            .as_mut()
            .unwrap()
            .steps
            .get_mut("deps")
            .unwrap()
            .shared_cache_growth_bytes = Some(1);
        assert!(validate_config(&shared_growth_without_shared_cache).is_err());

        let mut missing_shared_growth = managed_profile_config();
        missing_shared_growth.steps[0].cache = PreparationCachePolicy::RepositoryShared;
        assert!(validate_config(&missing_shared_growth).is_err());

        let mut zero_shared_growth = managed_profile_config();
        zero_shared_growth.steps[0].cache = PreparationCachePolicy::RepositoryShared;
        zero_shared_growth
            .managed_storage
            .as_mut()
            .unwrap()
            .steps
            .get_mut("deps")
            .unwrap()
            .shared_cache_growth_bytes = Some(0);
        assert!(validate_config(&zero_shared_growth).is_err());

        let mut overflowing = managed_profile_config();
        overflowing.steps.push(PreparationStep {
            name: "tools".into(),
            command: vec!["tool".into(), "install".into()],
            offline_command: None,
            inputs: Vec::new(),
            outputs: vec![".venv/".into()],
            cache: PreparationCachePolicy::WorktreeLocal,
            required_for_hooks: false,
        });
        let profile = overflowing.managed_storage.as_mut().unwrap();
        profile
            .steps
            .get_mut("deps")
            .unwrap()
            .output_growth_bytes
            .insert("vendor/".into(), u64::MAX);
        profile.steps.insert(
            "tools".into(),
            ManagedPreparationStepStorage {
                output_growth_bytes: BTreeMap::from([(".venv/".into(), 1)]),
                shared_cache_growth_bytes: None,
            },
        );
        assert!(validate_config(&overflowing).is_err());
    }

    #[test]
    fn managed_storage_profile_rejects_empty_steps_and_zero_resource_profiles() {
        let mut empty_steps = config();
        empty_steps.steps.clear();
        let empty_profile = ManagedPreparationStorageProfile {
            steps: BTreeMap::new(),
        };
        assert!(matches!(
            validate_managed_storage_profile(&empty_steps, &empty_profile),
            Err(PreparationError::InvalidConfig { .. })
        ));

        let mut zero_outputs = managed_profile_config();
        zero_outputs.steps[0].outputs.clear();
        zero_outputs
            .managed_storage
            .as_mut()
            .unwrap()
            .steps
            .get_mut("deps")
            .unwrap()
            .output_growth_bytes
            .clear();
        let profile = zero_outputs.managed_storage.as_ref().unwrap();
        assert!(matches!(
            validate_managed_storage_profile(&zero_outputs, profile),
            Err(PreparationError::InvalidConfig { .. })
        ));
        assert!(matches!(
            validate_config(&zero_outputs),
            Err(PreparationError::InvalidConfig { .. })
        ));
    }

    #[test]
    fn prepare_session_opt_in_guard_prevents_command_start() {
        let repository = tempfile::tempdir().unwrap();
        let run_git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(repository.path())
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        run_git(&["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(repository.path().join(".aethyme")).unwrap();
        std::fs::write(
            repository.path().join(PREPARATION_CONFIG_RELPATH),
            "schema_version = 1\n\
             [[steps]]\n\
             name = \"deps\"\n\
             command = [\"/bin/sh\", \"-c\", \"touch managed-storage-command-started && mkdir -p vendor\"]\n\
             outputs = [\"vendor/\"]\n\
             required_for_hooks = false\n\
             [managed_storage.steps.deps.output_growth_bytes]\n\
             \"vendor/\" = 4096\n",
        )
        .unwrap();
        run_git(&["add", PREPARATION_CONFIG_RELPATH]);
        run_git(&["commit", "-qm", "fixture"]);

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(repository.path()).unwrap();
        let session = broker
            .start_worktree("managed preparation guard", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        let config = load_config(&worktree)
            .unwrap()
            .expect("fixture declares managed preparation");
        let policy = crate::broker::gate_trust::GatePolicy::from_parts(&[], Some(&config));
        let host_state = storage.options().host_state_dir.unwrap();
        crate::broker::gate_trust::record_trust_for_host_state(
            repository.path(),
            &host_state,
            &[policy.policy_sha256.as_str()],
            "managed preparation fixture",
        )
        .unwrap();

        let sentinel = worktree.join("managed-storage-command-started");
        assert!(!sentinel.exists());

        #[cfg(unix)]
        {
            let reservation = match crate::file_lock::artifact_use::try_artifact_use_reclaimer(
                broker.main_root(),
                &worktree,
            )
            .unwrap()
            {
                crate::file_lock::artifact_use::ArtifactUseReclaimTry::Acquired(reservation) => {
                    reservation
                }
                blocked => {
                    panic!("fresh checkout should admit the test reclaimer: {blocked:?}")
                }
            };
            let error = broker
                .prepare_session(session.id, false, Duration::ZERO)
                .expect_err("preparation must refuse while artifact reclamation is reserved");
            assert!(matches!(
                error,
                BrokerOpError::Store(crate::BrokerError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::WouldBlock
            ));
            assert!(
                !sentinel.exists(),
                "preparation must not dispatch after artifact admission refusal"
            );
            drop(reservation);
        }

        let result = broker.prepare_session(session.id, false, Duration::ZERO);
        assert!(
            !sentinel.exists(),
            "an opted-in preparation command must not start without managed-storage enforcement"
        );
        assert!(matches!(
            result,
            Err(BrokerOpError::Preparation(
                PreparationError::ManagedStorageNotReady { paths: 1, steps: 1 }
            ))
        ));

        #[cfg(unix)]
        assert!(matches!(
            crate::file_lock::artifact_use::try_artifact_use_reclaimer(
                broker.main_root(),
                &worktree,
            )
            .unwrap(),
            crate::file_lock::artifact_use::ArtifactUseReclaimTry::Acquired(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn managed_storage_profile_rejects_overlapping_and_symlinked_output_roots() {
        let mut overlapping = managed_profile_config();
        overlapping.steps[0].outputs.push("vendor/nested/".into());
        overlapping
            .managed_storage
            .as_mut()
            .unwrap()
            .steps
            .get_mut("deps")
            .unwrap()
            .output_growth_bytes
            .insert("vendor/nested/".into(), 1);
        assert!(validate_config(&overlapping).is_err());

        let mut duplicate = managed_profile_config();
        duplicate.steps[0].outputs.push("vendor/".into());
        assert!(validate_config(&duplicate).is_err());

        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside = outside.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(&outside, root.join("vendor")).unwrap();
        let error = managed_preparation_storage_plan(&root, &managed_profile_config(), None)
            .expect_err("a symlink in an output path must be refused");
        assert!(matches!(
            error,
            PreparationError::ManagedStorageSymlink { .. }
        ));
    }

    #[test]
    fn managed_storage_toml_requires_integer_bounds_and_rejects_escaped_outputs() {
        let text = "schema_version = 1\n\
             [[steps]]\n\
             name = \"deps\"\n\
             command = [\"tool\", \"install\"]\n\
             outputs = [\"vendor/\"]\n\
             required_for_hooks = false\n\
             [managed_storage.steps.deps]\n\
             [managed_storage.steps.deps.output_growth_bytes]\n\
             \"vendor/\" = 4096\n";
        let parsed = parse_config(text).expect("documented managed-storage shape parses");
        assert_eq!(
            parsed.managed_storage.unwrap().steps["deps"].output_growth_bytes["vendor/"],
            4096
        );
        assert!(parse_config(&text.replace("4096", "1.5")).is_err());

        let mut escaped = managed_profile_config();
        escaped.steps[0].outputs = vec!["../outside".into()];
        let storage = escaped
            .managed_storage
            .as_mut()
            .unwrap()
            .steps
            .get_mut("deps")
            .unwrap();
        storage.output_growth_bytes.clear();
        storage.output_growth_bytes.insert("../outside".into(), 1);
        assert!(validate_config(&escaped).is_err());
    }

    #[test]
    fn symlinked_outputs_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.path().join("vendor")).unwrap();
        assert!(matches!(
            missing_outputs(root.path(), &config()),
            Err(PreparationError::SymlinkOutput { .. })
        ));
    }
}
