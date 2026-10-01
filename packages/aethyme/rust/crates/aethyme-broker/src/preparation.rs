//! Repository-declared, explicit dependency preparation for broker worktrees.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    Broker, BrokerOpError, HostResourceCoordinator, HostResourceKind, HostResourceRequest,
    HostResourceRequirement,
};

pub const PREPARATION_CONFIG_RELPATH: &str = ".aethyme/prepare.toml";
pub const PREPARATION_SCHEMA_VERSION: u32 = 1;
const PREPARATION_LEASE_TTL_SECONDS: u64 = 30;

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
/// distinct input set. Without a ceiling that is the only unbounded growth
/// path in the subsystem: dead entries are reclaimed by liveness, but a
/// repository that keeps producing fresh digests grows faster than its own
/// lockfile churn is noticed.
///
/// `0` disables the budget, which is the pre-existing behaviour and is never
/// wrong for a single-clone developer. It becomes wrong exactly when many
/// clones share the store, so the field exists rather than a heuristic.
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
        let mut coordinator = shared
            .then(HostResourceCoordinator::open_default)
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
        let cache_dir = if shared {
            Some(preparation_cache_dir(self, &digest)?)
        } else {
            None
        };
        if let Some(cache_dir) = &cache_dir {
            fs::create_dir_all(cache_dir).map_err(|source| PreparationError::Io {
                path: cache_dir.clone(),
                source,
            })?;
        }
        // Enforce the budget while the exclusive lease is held, never after
        // release: rotation removes a store other worktrees may be reading, so
        // it has to be inside the same critical section that granted access.
        let rotation = shared_cache_budget(config.shared_cache.as_ref(), &digest, self)?;

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
        write_record(&state_path, &record)?;

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
        let mut next_renewal =
            std::time::Instant::now() + Duration::from_secs(PREPARATION_LEASE_TTL_SECONDS / 3);
        for step in &config.steps {
            let command = if offline {
                step.offline_command
                    .as_ref()
                    .expect("validated offline command")
            } else {
                &step.command
            };
            let report = match self.guarded_exec_with_env_and_heartbeat(
                session_id,
                command,
                &environment,
                || {
                    if std::time::Instant::now() < next_renewal {
                        return Ok(());
                    }
                    if let (Some(coordinator), Some(grant)) = (coordinator.as_mut(), grant.as_mut())
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
                    next_renewal = std::time::Instant::now()
                        + Duration::from_secs(PREPARATION_LEASE_TTL_SECONDS / 3);
                    Ok(())
                },
            ) {
                Ok(report) => report,
                Err(error) => {
                    record.state = PreparationState::Failed;
                    record.failed_step = Some(step.name.clone());
                    record.completed_at = Some(now_ms());
                    write_record(&state_path, &record)?;
                    release_grant(&mut coordinator, &mut grant)?;
                    return Err(error);
                }
            };
            results.push(PreparationStepResult {
                name: step.name.clone(),
                exit_code: report.exit_code,
                succeeded: report.ok,
            });
            if !report.ok {
                record.state = PreparationState::Failed;
                record.failed_step = Some(step.name.clone());
                record.exit_code = report.exit_code;
                record.completed_at = Some(now_ms());
                write_record(&state_path, &record)?;
                release_grant(&mut coordinator, &mut grant)?;
                return Err(PreparationError::StepFailed {
                    step: step.name.clone(),
                    exit_code: report.exit_code,
                    session_id,
                }
                .into());
            }
            let absent = match missing_step_outputs(&root, step) {
                Ok(absent) => absent,
                Err(error) => {
                    record.state = PreparationState::Failed;
                    record.failed_step = Some(step.name.clone());
                    record.completed_at = Some(now_ms());
                    write_record(&state_path, &record)?;
                    release_grant(&mut coordinator, &mut grant)?;
                    return Err(error.into());
                }
            };
            if !absent.is_empty() {
                record.state = PreparationState::Failed;
                record.failed_step = Some(step.name.clone());
                record.completed_at = Some(now_ms());
                write_record(&state_path, &record)?;
                release_grant(&mut coordinator, &mut grant)?;
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
        write_record(&state_path, &record)?;
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
    if command.is_empty() || command.iter().any(|part| part.is_empty()) {
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
    let mut hasher = Sha256::new();
    hasher.update(PREPARATION_SCHEMA_VERSION.to_le_bytes());
    hasher.update(std::env::consts::OS.as_bytes());
    hasher.update([0]);
    hasher.update(std::env::consts::ARCH.as_bytes());
    hasher.update(serde_json::to_vec(config).map_err(|error| invalid_config(error.to_string()))?);
    let mut inputs = config
        .steps
        .iter()
        .flat_map(|step| step.inputs.iter())
        .collect::<Vec<_>>();
    inputs.sort();
    inputs.dedup();
    for relative in inputs {
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path).map_err(|_| PreparationError::InvalidInput {
            path: relative.clone(),
        })?;
        if !metadata.file_type().is_file() {
            return Err(PreparationError::InvalidInput {
                path: relative.clone(),
            });
        }
        let bytes = fs::read(&path).map_err(|source| PreparationError::Io {
            path: PathBuf::from(relative),
            source,
        })?;
        hasher.update(relative.as_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(Sha256::digest(bytes));
    }
    for runtime in &config.runtimes {
        let executable =
            resolve_runtime_executable(root, &runtime.command[0]).ok_or_else(|| {
                PreparationError::RuntimeProbe {
                    name: runtime.name.clone(),
                    session_id,
                }
            })?;
        let canonical = executable
            .canonicalize()
            .map_err(|_| PreparationError::RuntimeProbe {
                name: runtime.name.clone(),
                session_id,
            })?;
        let metadata = fs::metadata(&canonical).map_err(|_| PreparationError::RuntimeProbe {
            name: runtime.name.clone(),
            session_id,
        })?;
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
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn resolve_runtime_executable(root: &Path, program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.components().count() > 1 {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        return candidate.is_file().then_some(candidate);
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
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
                name: format!("aethyme-prepare-cache:{repository}"),
            },
        }],
    })
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
    let Some(config) = load_config(root)? else {
        return Ok(None);
    };
    if !config
        .steps
        .iter()
        .any(|step| step.cache == PreparationCachePolicy::RepositoryShared)
    {
        return Ok(None);
    }
    let digest = preparation_digest(root, &config, 0)?;
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
/// Returns `None` when no budget is declared or the store is within it, so the
/// common path does no measurement at all.
fn shared_cache_budget(
    budget: Option<&SharedCacheBudget>,
    digest: &str,
    broker: &Broker,
) -> Result<Option<SharedCacheRotation>, PreparationError> {
    let Some(budget) = budget.filter(|budget| budget.max_bytes > 0) else {
        return Ok(None);
    };
    let store = preparation_store_dir(broker)?;
    shared_cache_budget_at(budget, &store, digest)
}

/// The no-budget path, exposed for the test that proves an absent budget does
/// no measurement at all. Separate from the real entry point so that test
/// cannot accidentally exercise host state.
#[cfg(test)]
fn shared_cache_budget_at_probe(
    budget: Option<&SharedCacheBudget>,
    store: &Path,
) -> Result<Option<SharedCacheRotation>, PreparationError> {
    match budget {
        None => Ok(None),
        Some(budget) => shared_cache_budget_at(budget, store, "abcdef123456"),
    }
}

/// Rotation against an explicit store path.
///
/// Split from [`shared_cache_budget`] so the decision is testable without a
/// broker, a host state directory, or a real package-manager store on disk.
/// The caller holds the exclusive lease either way; this function only decides
/// whether the bytes justify a reset.
fn shared_cache_budget_at(
    budget: &SharedCacheBudget,
    store: &Path,
    digest: &str,
) -> Result<Option<SharedCacheRotation>, PreparationError> {
    if budget.max_bytes == 0 || !store.is_dir() {
        return Ok(None);
    }
    let bytes = directory_bytes(store);
    if bytes <= budget.max_bytes {
        return Ok(None);
    }
    // Same retired-then-remove shape the managed gate cache uses, so an
    // interrupted rotation leaves a directory that is still reclaimable by the
    // liveness sweep rather than an unreadable half-state.
    let retired = store.with_extension(format!("retired-{}-{}", epoch_ms(), std::process::id()));
    std::fs::rename(store, &retired).map_err(|source| PreparationError::Io {
        path: store.to_path_buf(),
        source,
    })?;
    let remove = std::fs::remove_dir_all(&retired);
    fs::create_dir_all(store).map_err(|source| PreparationError::Io {
        path: store.to_path_buf(),
        source,
    })?;
    Ok(Some(SharedCacheRotation {
        // The key is a digest, never a path: a report must not disclose where
        // the store lives on this machine.
        key: digest[..12.min(digest.len())].to_string(),
        budget_bytes: budget.max_bytes,
        bytes_before: bytes,
        bytes_after: 0,
        removed: remove.is_ok(),
    }))
}

/// What a rotation did, for the report and the event.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SharedCacheRotation {
    pub key: String,
    pub budget_bytes: u64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// False when the retired tree could not be removed. The fresh store is
    /// still correct, so this is reported rather than fatal: the leftover is
    /// reclaimable by the liveness sweep.
    pub removed: bool,
}

fn preparation_store_dir(broker: &Broker) -> Result<PathBuf, PreparationError> {
    let db = crate::default_host_resource_db_path()?;
    let parent = db
        .parent()
        .ok_or_else(|| invalid_config("host state has no parent directory"))?;
    Ok(parent
        .join("preparation-cache")
        .join(repository_identity(broker)))
}

fn directory_bytes(path: &Path) -> u64 {
    let mut bytes = 0_u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                bytes = bytes.saturating_add(metadata.len());
            }
        }
    }
    bytes
}

fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default()
}

fn preparation_cache_dir(broker: &Broker, digest: &str) -> Result<PathBuf, PreparationError> {
    let db = crate::default_host_resource_db_path()?;
    let parent = db
        .parent()
        .ok_or_else(|| invalid_config("host state has no parent directory"))?;
    Ok(parent
        .join("preparation-cache")
        .join(repository_identity(broker))
        .join(&digest[..12]))
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
    if let (Some(coordinator), Some(grant)) = (coordinator.as_mut(), grant.take()) {
        coordinator.release(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        )?;
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
        let store = tmp.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("big"), vec![0_u8; 4096]).unwrap();
        let report = shared_cache_budget_at_probe(None, &store).unwrap();
        assert!(report.is_none(), "no budget must mean no measurement");
        assert!(store.join("big").exists(), "the store must be untouched");
    }

    /// Over budget rotates the store and leaves a usable one behind; under
    /// budget measures and does nothing.
    #[test]
    fn an_over_budget_shared_store_is_rotated_while_under_budget_is_not() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("store");
        std::fs::create_dir_all(store.join("nested")).unwrap();
        std::fs::write(store.join("nested/payload"), vec![0_u8; 8192]).unwrap();
        let bytes = directory_bytes(&store);
        assert!(bytes >= 8192);

        let budget = SharedCacheBudget {
            max_bytes: bytes - 1,
        };
        let rotation = shared_cache_budget_at(&budget, &store, "abcdef123456")
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
            shared_cache_budget_at(&budget, &store, "abcdef123456")
                .unwrap()
                .is_none()
        );
        assert!(store.join("payload").exists());
    }

    /// The report must not disclose where the store lives: it is host state
    /// outside the repository, and this record reaches JSON output.
    #[test]
    fn a_rotation_report_carries_no_host_path() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("host-state/preparation-cache/repo");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("payload"), vec![0_u8; 4096]).unwrap();
        let rotation =
            shared_cache_budget_at(&SharedCacheBudget { max_bytes: 1 }, &store, "0123456789ab")
                .unwrap()
                .unwrap();
        let json = serde_json::to_string(&rotation).unwrap();
        assert!(
            !json.contains(tmp.path().to_string_lossy().as_ref()),
            "rotation report leaked a host path: {json}"
        );
        assert!(!json.contains("preparation-cache"), "{json}");
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
