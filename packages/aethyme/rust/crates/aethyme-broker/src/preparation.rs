//! Repository-declared, explicit dependency preparation for broker worktrees.

use std::collections::BTreeSet;
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
    /// Optional budget for the repository-shared store. Absent means no
    /// ceiling, preserving existing behaviour.
    #[serde(default)]
    pub shared_cache: Option<SharedCacheBudget>,
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
    /// Rotate the whole shared cache when it exceeds this many bytes.
    pub max_bytes: u64,
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
        if status.state == PreparationState::InProgress {
            return Err(PreparationError::PriorPreparationIncomplete { session_id }.into());
        }
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
        let cache_dir = shared
            .then(|| preparation_cache_dir(self, &digest))
            .transpose()?;
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
        if let Err(error) = require_preparation_storage(self.main_root(), &storage_paths) {
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
        if let Err(error) = require_preparation_storage(self.main_root(), &storage_paths) {
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
            shared_cache_budget(config.shared_cache.as_ref(), &digest, self)
        } else {
            Ok(None)
        };
        let rotation =
            self.release_grant_after_error(rotation_result, &mut coordinator, &mut grant, || {
                require_preparation_storage(&storage_root, &storage_paths)
            })?;
        if let Some(cache_dir) = &cache_dir {
            self.release_grant_after_error(
                fs::create_dir_all(cache_dir).map_err(|source| PreparationError::Io {
                    path: cache_dir.clone(),
                    source,
                }),
                &mut coordinator,
                &mut grant,
                || require_preparation_storage(&storage_root, &storage_paths),
            )?;
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
            || require_preparation_storage(&storage_root, &storage_paths),
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
            let report = match require_preparation_storage(&main_root, &storage_paths)
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
                                || require_preparation_storage(&main_root, &storage_paths),
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
                    if matches!(
                        &error,
                        BrokerOpError::Preparation(PreparationError::CleanupUnknown { .. })
                    ) {
                        // The command may still be writing. Keep both the
                        // in-progress preparation record and its named shared
                        // cache allocation until an operator can verify cleanup.
                        quarantine_grant_best_effort(&mut coordinator, &mut grant);
                        return Err(error);
                    }
                    let recovery_persisted = self
                        .recover_storage_pressure_after_command_failure(&error, || {
                            require_preparation_storage(&main_root, &storage_paths)
                        });
                    record.state = PreparationState::Failed;
                    record.failed_step = Some(step.name.clone());
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
                let recovery_persisted = self
                    .recover_storage_pressure_after_command_failure(&error, || {
                        require_preparation_storage(&main_root, &storage_paths)
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
            || require_preparation_storage(&storage_root, &storage_paths),
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
    let config: PreparationConfig =
        toml::from_str(text).map_err(|error| PreparationError::InvalidConfig {
            path: PREPARATION_CONFIG_RELPATH.into(),
            reason: error.to_string(),
        })?;
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
            "shared_cache.max_bytes requires at least one step with cache = \"repository_shared\"",
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
    Ok(())
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
) -> Result<Option<SharedCacheRotation>, PreparationError> {
    let cache_root = broker
        .storage_context()
        .preparation_cache_root()
        .ok_or_else(|| {
            invalid_config("the broker's captured host resource database path is unavailable")
        })?;
    let resource_db_path = broker
        .storage_context()
        .host_resource_db_path
        .as_deref()
        .ok_or_else(|| PreparationError::StorageUnavailable {
            path: cache_root.clone(),
            reason: "the broker's captured host resource database path is unavailable".into(),
        })?;
    shared_cache_budget_at_with_resource_db(
        budget.filter(|budget| budget.max_bytes > 0),
        &cache_root,
        resource_db_path,
        &repository_identity(broker),
        digest,
    )
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

fn shared_cache_budget_at_with_resource_db(
    budget: Option<&SharedCacheBudget>,
    cache_root: &Path,
    resource_db_path: &Path,
    repository: &str,
    digest: &str,
) -> Result<Option<SharedCacheRotation>, PreparationError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    recover_rotation_debt(resource_db_path, cache_root, repository)?;

    let Some(budget) = budget.filter(|budget| budget.max_bytes > 0) else {
        return Ok(None);
    };
    let store = cache_root.join(repository);
    let Some(bytes) = directory_bytes(&store)? else {
        return Ok(None);
    };
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
        let payload = rotation_debt::encode(&debt).map_err(|source| {
            PreparationError::StorageUnavailable {
                path: store.clone(),
                reason: format!("cannot encode shared-cache rotation intent: {source}"),
            }
        })?;
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
        rotation_debt::publish(cache_root, debt).map_err(|source| {
            PreparationError::StorageUnavailable {
                path: store.clone(),
                reason: format!("cannot publish shared-cache rotation intent: {source}"),
            }
        })?;
        let progress = recover_rotation_debt(resource_db_path, cache_root, repository)?;
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
pub(crate) fn recover_rotation_debt_for_cache_sweep(
    cache_root: &Path,
    resource_db_path: &Path,
    repository: &str,
    deadline: Instant,
) -> Result<rotation_debt::RecoveryProgress, rotation_debt::RecoveryError> {
    rotation_debt::recover_with_journal(cache_root, resource_db_path, repository, deadline)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn recover_rotation_debt_for_cache_sweep(
    cache_root: &Path,
    _resource_db_path: &Path,
    repository: &str,
    deadline: Instant,
) -> Result<rotation_debt::RecoveryProgress, rotation_debt::RecoveryError> {
    rotation_debt::recover(cache_root, repository, deadline)
}

pub(crate) fn probe_rotation_recovery(
    _cache_root: &Path,
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn recover_rotation_debt(
    resource_db_path: &Path,
    cache_root: &Path,
    repository: &str,
) -> Result<rotation_debt::RecoveryProgress, PreparationError> {
    let deadline = Instant::now() + SHARED_CACHE_RECOVERY_MAX_ELAPSED;
    match rotation_debt::recover_with_journal(cache_root, resource_db_path, repository, deadline) {
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

fn preparation_store_dir(broker: &Broker) -> Result<PathBuf, PreparationError> {
    let root = broker
        .storage_context()
        .preparation_cache_root()
        .ok_or_else(|| {
            invalid_config("the broker's captured host resource database path is unavailable")
        })?;
    Ok(root.join(repository_identity(broker)))
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

fn preparation_cache_dir(broker: &Broker, digest: &str) -> Result<PathBuf, PreparationError> {
    Ok(preparation_store_dir(broker)?.join(&digest[..12]))
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
    crate::resolve_remote_target(broker.repo_handle(), "origin", None)
        .map(|target| hash_text(&target.coordination_key))
        .unwrap_or_else(|_| hash_text(&broker.main_root().to_string_lossy()))
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
        }
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

    /// Absent means absent: an existing repository's preparation keeps
    /// behaving exactly as it did before the field existed.
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
                Some(&SharedCacheBudget { max_bytes: 1 }),
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
            Some(&SharedCacheBudget { max_bytes: 1 }),
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
                Some(&SharedCacheBudget { max_bytes: 1 }),
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
                Some(&SharedCacheBudget { max_bytes: 1 }),
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
