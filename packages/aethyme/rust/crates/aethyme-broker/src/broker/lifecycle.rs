use super::*;
use sha2::{Digest, Sha256};
use std::time::Duration;

const BROKER_OPEN_HOST_OPERATION_DB_ENV: &str = "AETHYME_BROKER_OPEN_HOST_OPERATION_DB";
const BROKER_OPEN_HOST_STATE_EXPLICIT_ENV: &str = "AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT";
const BROKER_OPEN_WORKTREE_ROOT_ENV: &str = "AETHYME_BROKER_OPEN_WORKTREE_ROOT";
const BROKER_OPEN_COLLABORATION_ROOT_ENV: &str = "AETHYME_BROKER_OPEN_COLLABORATION_ROOT";
// Keep the private transport key stable; the payload version rejects old children explicitly.
const BROKER_OPEN_STORAGE_CONTEXT_ENV: &str = "AETHYME_BROKER_STORAGE_CONTEXT_V1";
const PREPARATION_CACHE_ROOT_ENV: &str = "AETHYME_PREPARATION_CACHE_ROOT";
pub(crate) const WORKTREE_VOLUME_PREPARATION_CACHE_ROOT: &str = ".aethyme-preparation-cache";
const BROKER_OPEN_STORAGE_CONTEXT_LEGACY_VERSION: u32 = 2;
const BROKER_OPEN_STORAGE_CONTEXT_V4_VERSION: u32 = 4;
const BROKER_OPEN_STORAGE_CONTEXT_MAX_BYTES: usize = 32 * 1024;
const ARTIFACT_SWEEP_NAMESPACE_V2_DOMAIN: &[u8] = b"aethyme-artifact-sweep-context-v2\0";
const ARTIFACT_SWEEP_NAMESPACE_V4_DOMAIN: &[u8] = b"aethyme-artifact-sweep-context-v4\0";

fn timed_start_phase<T>(
    phases: &mut std::collections::BTreeMap<String, u64>,
    name: &'static str,
    run: impl FnOnce() -> T,
) -> T {
    let started = std::time::Instant::now();
    let result = run();
    phases.insert(
        name.into(),
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
    result
}

struct CreatedSessionWorktree {
    phase_timings_ms: std::collections::BTreeMap<String, u64>,
    slug: String,
    branch: String,
    start_base: SessionStartBase,
    worktree: GitRepo,
    worktree_placement: WorktreePlacement,
    selected_promote_branch: String,
    _lifecycle_claim: crate::file_lock::ExclusiveFileLock,
}

fn capture_spawned_worktree_instance_witness(
    worktree: &crate::git::GitRepo,
) -> Option<crate::events::SpawnedWorktreeInstanceWitness> {
    // Initialization returns only after the marker is synced and read back.
    // If the filesystem cannot persist it, leave this session ineligible for
    // unattended cleanup while allowing normal worktree use and finish.
    let initialized_generation = crate::root_generation::initialize(worktree.root())?;
    let witness = worktree.spawned_worktree_instance_witness()?;
    (witness.root_generation_sha256.as_deref() == Some(initialized_generation.as_str()))
        .then_some(witness)
}

#[derive(Clone)]
struct ReusePrefetchedDefaultBranch {
    session_id: i64,
    drift: Option<crate::DefaultBranchDrift>,
    note: Option<String>,
    refresh: Option<crate::session_push::DefaultBranchRefreshEvidence>,
}

struct ReuseFreshnessDecision {
    report: crate::AdoptBaseFreshness,
    observation: crate::broker::SessionBaseObservation,
    refresh_diff_base: Option<String>,
    checkout_moved_to: Option<String>,
    integration_sync: Option<AdoptIntegrationSync>,
}

struct ReuseTarget {
    reference: String,
    commit: String,
    evidence: Option<crate::SessionStartBaseEvidence>,
    default_ref: Option<String>,
    promote_branch: String,
    verify_ref: bool,
}

fn reuse_freshness_refusal(reason: impl Into<String>) -> BrokerOpError {
    BrokerOpError::SessionSyncRefused {
        reason: reason.into(),
    }
}

fn is_full_commit_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    <Option<T> as serde::Deserialize>::deserialize(deserializer)
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturedBrokerStorageContextV2 {
    version: u32,
    repository_root: PathBuf,
    git_common_dir: PathBuf,
    #[serde(deserialize_with = "deserialize_required_option")]
    host_state_dir: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_required_option")]
    host_resource_db_path: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_required_option")]
    host_cache_dir: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_required_option")]
    host_operation_db_path: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_required_option")]
    worktree_root_override: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_required_option")]
    worktree_root_environment_override: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_required_option")]
    collaboration_state_dir: Option<PathBuf>,
    host_state_dir_is_explicit: bool,
}

impl CapturedBrokerStorageContextV2 {
    fn from_context(context: &BrokerStorageContext) -> Result<Self, String> {
        context.ensure_absolute_paths()?;
        if context.preparation_cache_root_override.is_some()
            || context.preparation_cache_root_identity.is_some()
            || context.preparation_cache_root_state.is_some()
            || context.preparation_cache_root_handle.is_some()
        {
            return Err("the legacy V2 context cannot encode a preparation-cache override".into());
        }
        let repository_root = context
            .repository_root
            .clone()
            .ok_or_else(|| "child context has no repository root binding".to_string())?;
        let git_common_dir = context
            .git_common_dir
            .clone()
            .ok_or_else(|| "child context has no Git common-directory binding".to_string())?;
        Ok(Self {
            version: BROKER_OPEN_STORAGE_CONTEXT_LEGACY_VERSION,
            repository_root,
            git_common_dir,
            host_state_dir: context.host_state_dir.clone(),
            host_resource_db_path: context.host_resource_db_path.clone(),
            host_cache_dir: context.host_cache_dir.clone(),
            host_operation_db_path: context.host_operation_db_path.clone(),
            worktree_root_override: context.worktree_root_override.clone(),
            worktree_root_environment_override: context.worktree_root_environment_override.clone(),
            collaboration_state_dir: context.collaboration_state_dir.clone(),
            host_state_dir_is_explicit: context.host_state_dir_is_explicit,
        })
    }

    fn into_context(self) -> Result<BrokerStorageContext, String> {
        if self.version != BROKER_OPEN_STORAGE_CONTEXT_LEGACY_VERSION {
            return Err(format!(
                "unsupported child-context version {}; expected {}",
                self.version, BROKER_OPEN_STORAGE_CONTEXT_LEGACY_VERSION
            ));
        }
        let context = BrokerStorageContext {
            repository_root: Some(self.repository_root),
            git_common_dir: Some(self.git_common_dir),
            host_state_dir: self.host_state_dir,
            host_resource_db_path: self.host_resource_db_path,
            host_cache_dir: self.host_cache_dir,
            host_operation_db_path: self.host_operation_db_path,
            worktree_root_override: self.worktree_root_override,
            worktree_root_environment_override: self.worktree_root_environment_override,
            collaboration_state_dir: self.collaboration_state_dir,
            preparation_cache_root_override: None,
            preparation_cache_root_identity: None,
            preparation_cache_root_state: None,
            preparation_cache_root_handle: None,
            host_state_dir_is_explicit: self.host_state_dir_is_explicit,
            capture_error: None,
        };
        context.ensure_absolute_paths()?;
        if context.worktree_root_override.is_some()
            && context.worktree_root_environment_override.is_some()
        {
            return Err("child context contains both exact and environment worktree roots".into());
        }
        let expected_resource_db = context
            .host_state_dir
            .as_deref()
            .map(|directory| directory.join("host-resources.db"));
        if context.host_resource_db_path != expected_resource_db {
            return Err(
                "child context host resource database does not match its state root".into(),
            );
        }
        Ok(context)
    }

    fn encode(context: &BrokerStorageContext) -> Result<String, BrokerOpError> {
        let payload =
            Self::from_context(context).map_err(|reason| BrokerOpError::InvalidStorageContext {
                reason: reason.into_boxed_str(),
            })?;
        let encoded = serde_json::to_string(&payload).map_err(|error| {
            BrokerOpError::InvalidStorageContext {
                reason: format!("cannot encode child context: {error}").into_boxed_str(),
            }
        })?;
        if encoded.len() > BROKER_OPEN_STORAGE_CONTEXT_MAX_BYTES {
            return Err(BrokerOpError::InvalidStorageContext {
                reason: "child context exceeds its size bound".into(),
            });
        }
        Ok(encoded)
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturedBrokerStorageContextV4 {
    version: u32,
    repository_root: PathBuf,
    git_common_dir: PathBuf,
    #[serde(deserialize_with = "deserialize_required_option")]
    host_state_dir: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_required_option")]
    host_resource_db_path: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_required_option")]
    host_cache_dir: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_required_option")]
    host_operation_db_path: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_required_option")]
    worktree_root_override: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_required_option")]
    worktree_root_environment_override: Option<PathBuf>,
    #[serde(deserialize_with = "deserialize_required_option")]
    collaboration_state_dir: Option<PathBuf>,
    preparation_cache_root: PathBuf,
    preparation_cache_root_identity: Vec<u8>,
    preparation_cache_nonce: String,
    preparation_cache_namespace_identity: Vec<u8>,
    preparation_cache_coordinator_db_path: PathBuf,
    host_state_dir_is_explicit: bool,
}

impl CapturedBrokerStorageContextV4 {
    fn from_context(context: &BrokerStorageContext) -> Result<Self, String> {
        context
            .ensure_absolute_paths()
            .map_err(|error| error.to_string())?;
        context
            .validate_preparation_cache_root_override()
            .map_err(|error| error.to_string())?;
        let repository_root = context
            .repository_root
            .clone()
            .ok_or_else(|| "child context has no repository root binding".to_string())?;
        let git_common_dir = context
            .git_common_dir
            .clone()
            .ok_or_else(|| "child context has no Git common-directory binding".to_string())?;
        let root = context
            .preparation_cache_root_override
            .clone()
            .ok_or_else(|| "V4 child context has no preparation-cache override".to_string())?;
        let root_identity = context
            .preparation_cache_root_identity
            .clone()
            .ok_or_else(|| "V4 child context has no captured root identity".to_string())?;
        let cache_identity = match context.preparation_cache_root_state.as_ref() {
            Some(crate::broker::PreparationCacheRootState::Marked {
                coordinator_db_path,
                nonce,
                cache_root_identity: Some(identity),
            }) => {
                let captured_resource_db =
                    context.host_resource_db_path.as_deref().ok_or_else(|| {
                        "V4 child context has no captured host-resource database".to_string()
                    })?;
                let canonical = canonical_coordinator_db_path(captured_resource_db)?;
                if &canonical != coordinator_db_path {
                    return Err("V4 child context host-resource database differs from the preparation-cache root binding".into());
                }
                (nonce.clone(), identity.clone(), coordinator_db_path.clone())
            }
            _ => {
                return Err(
                    "an uninitialized or incomplete preparation-cache root cannot be sent to a child"
                        .into(),
                );
            }
        };
        if context.preparation_cache_root_handle.is_none() {
            return Err("V4 child context has no retained preparation-cache root handle".into());
        }
        Ok(Self {
            version: BROKER_OPEN_STORAGE_CONTEXT_V4_VERSION,
            repository_root,
            git_common_dir,
            host_state_dir: context.host_state_dir.clone(),
            host_resource_db_path: context.host_resource_db_path.clone(),
            host_cache_dir: context.host_cache_dir.clone(),
            host_operation_db_path: context.host_operation_db_path.clone(),
            worktree_root_override: context.worktree_root_override.clone(),
            worktree_root_environment_override: context.worktree_root_environment_override.clone(),
            collaboration_state_dir: context.collaboration_state_dir.clone(),
            preparation_cache_root: root,
            preparation_cache_root_identity: root_identity,
            preparation_cache_nonce: cache_identity.0,
            preparation_cache_namespace_identity: cache_identity.1,
            preparation_cache_coordinator_db_path: cache_identity.2,
            host_state_dir_is_explicit: context.host_state_dir_is_explicit,
        })
    }

    fn into_context(self) -> Result<BrokerStorageContext, String> {
        if self.version != BROKER_OPEN_STORAGE_CONTEXT_V4_VERSION {
            return Err(format!(
                "unsupported child-context version {}; expected {}",
                self.version, BROKER_OPEN_STORAGE_CONTEXT_V4_VERSION
            ));
        }
        let (canonical, root_identity, state, directory) =
            capture_preparation_cache_root(&self.preparation_cache_root)?;
        if canonical != self.preparation_cache_root
            || root_identity != self.preparation_cache_root_identity
        {
            return Err("V4 child context preparation-cache root identity changed".into());
        }
        match &state {
            crate::broker::PreparationCacheRootState::Marked {
                coordinator_db_path,
                nonce,
                cache_root_identity: Some(identity),
            } if nonce == &self.preparation_cache_nonce
                && identity == &self.preparation_cache_namespace_identity
                && coordinator_db_path == &self.preparation_cache_coordinator_db_path => {}
            _ => {
                return Err(
                    "V4 child context marker or nonce namespace is missing or changed".into(),
                );
            }
        }
        let context = BrokerStorageContext {
            repository_root: Some(self.repository_root),
            git_common_dir: Some(self.git_common_dir),
            host_state_dir: self.host_state_dir,
            host_resource_db_path: self.host_resource_db_path,
            host_cache_dir: self.host_cache_dir,
            host_operation_db_path: self.host_operation_db_path,
            worktree_root_override: self.worktree_root_override,
            worktree_root_environment_override: self.worktree_root_environment_override,
            collaboration_state_dir: self.collaboration_state_dir,
            preparation_cache_root_override: Some(canonical),
            preparation_cache_root_identity: Some(root_identity),
            preparation_cache_root_state: Some(state),
            preparation_cache_root_handle: Some(directory),
            host_state_dir_is_explicit: self.host_state_dir_is_explicit,
            capture_error: None,
        };
        context.ensure_absolute_paths()?;
        if context.worktree_root_override.is_some()
            && context.worktree_root_environment_override.is_some()
        {
            return Err("child context contains both exact and environment worktree roots".into());
        }
        let expected_resource_db = context
            .host_state_dir
            .as_deref()
            .map(|directory| directory.join("host-resources.db"));
        if context.host_resource_db_path != expected_resource_db {
            return Err(
                "child context host resource database does not match its state root".into(),
            );
        }
        let captured_resource_db = context
            .host_resource_db_path
            .as_deref()
            .ok_or_else(|| "V4 child context has no captured host-resource database".to_string())?;
        if canonical_coordinator_db_path(captured_resource_db)?
            != self.preparation_cache_coordinator_db_path
        {
            return Err(
                "V4 child context host-resource database binding does not match its captured path"
                    .into(),
            );
        }
        context
            .validate_preparation_cache_root_override()
            .map_err(|error| error.to_string())?;
        Ok(context)
    }

    fn encode(context: &BrokerStorageContext) -> Result<String, BrokerOpError> {
        let payload =
            Self::from_context(context).map_err(|reason| BrokerOpError::InvalidStorageContext {
                reason: reason.into_boxed_str(),
            })?;
        let encoded = serde_json::to_string(&payload).map_err(|error| {
            BrokerOpError::InvalidStorageContext {
                reason: format!("cannot encode child context: {error}").into_boxed_str(),
            }
        })?;
        if encoded.len() > BROKER_OPEN_STORAGE_CONTEXT_MAX_BYTES {
            return Err(BrokerOpError::InvalidStorageContext {
                reason: "child context exceeds its size bound".into(),
            });
        }
        Ok(encoded)
    }
}

impl BrokerStorageContext {
    /// Stable identifier for maintenance state owned by this exact captured
    /// storage context. The legacy context remains byte-for-byte stable when
    /// the separate preparation-cache root is unset.
    pub(crate) fn artifact_sweep_namespace(&self) -> Result<String, BrokerOpError> {
        self.ensure_valid()?;
        if self.preparation_cache_root_override.is_some() {
            let encoded = CapturedBrokerStorageContextV4::encode(self)?;
            let mut digest = Sha256::new();
            digest.update(ARTIFACT_SWEEP_NAMESPACE_V4_DOMAIN);
            digest.update(encoded.as_bytes());
            return Ok(format!("{:x}", digest.finalize()));
        }
        let encoded = CapturedBrokerStorageContextV2::encode(self)?;
        let mut digest = Sha256::new();
        digest.update(ARTIFACT_SWEEP_NAMESPACE_V2_DOMAIN);
        digest.update(encoded.as_bytes());
        Ok(format!("{:x}", digest.finalize()))
    }

    fn capture(options: BrokerOpenOptions) -> Self {
        if let Some(encoded) = std::env::var_os(BROKER_OPEN_STORAGE_CONTEXT_ENV) {
            if options.host_state_dir.is_some()
                || options.host_cache_dir.is_some()
                || options.host_operation_db_path.is_some()
                || options.worktree_root.is_some()
                || options.collaboration_state_dir.is_some()
                || options.preparation_cache_root.is_some()
            {
                return Self::invalid(
                    "an explicit BrokerOpenOptions value cannot be combined with a child context",
                );
            }
            let Some(encoded) = encoded.to_str() else {
                return Self::invalid("child context is not valid UTF-8");
            };
            if encoded.len() > BROKER_OPEN_STORAGE_CONTEXT_MAX_BYTES {
                return Self::invalid("child context exceeds its size bound");
            }
            let payload = match serde_json::from_str::<serde_json::Value>(encoded) {
                Ok(payload) => payload,
                Err(error) => {
                    return Self::invalid(format!("child context is malformed: {error}"));
                }
            };
            let version = match payload.get("version").and_then(serde_json::Value::as_u64) {
                Some(version) => version,
                None => return Self::invalid("child context has no supported version"),
            };
            return match version {
                value if value == u64::from(BROKER_OPEN_STORAGE_CONTEXT_LEGACY_VERSION) => {
                    match serde_json::from_str::<CapturedBrokerStorageContextV2>(encoded) {
                        Ok(decoded) => decoded.into_context().unwrap_or_else(Self::invalid),
                        Err(error) => Self::invalid(format!("child context is malformed: {error}")),
                    }
                }
                value if value == u64::from(BROKER_OPEN_STORAGE_CONTEXT_V4_VERSION) => {
                    match serde_json::from_str::<CapturedBrokerStorageContextV4>(encoded) {
                        Ok(decoded) => decoded.into_context().unwrap_or_else(Self::invalid),
                        Err(error) => Self::invalid(format!("child context is malformed: {error}")),
                    }
                }
                _ => Self::invalid(format!("unsupported child-context version {version}")),
            };
        }

        let explicit_host_state_dir = options.host_state_dir.is_some();
        let inherited_explicit_host_state =
            match std::env::var_os(BROKER_OPEN_HOST_STATE_EXPLICIT_ENV)
                .and_then(|value| value.into_string().ok())
                .as_deref()
            {
                Some("true") => true,
                Some("false") => false,
                _ => crate::host_state::host_state_dir_is_explicit(),
            };
        let preparation_cache_root_override = options
            .preparation_cache_root
            .or_else(|| environment_path(PREPARATION_CACHE_ROOT_ENV));
        let (
            preparation_cache_root_override,
            preparation_cache_root_identity,
            preparation_cache_root_state,
            preparation_cache_root_handle,
        ) = match preparation_cache_root_override {
            Some(path) => match capture_preparation_cache_root(&path) {
                Ok((canonical, identity, state, directory)) => (
                    Some(canonical),
                    Some(identity),
                    Some(state),
                    Some(directory),
                ),
                Err(reason) => return Self::invalid(reason),
            },
            None => (None, None, None, None),
        };
        let host_state_dir = options
            .host_state_dir
            .or_else(crate::host_state::default_host_state_dir);
        let host_resource_db_path = host_state_dir
            .as_ref()
            .map(|directory| directory.join("host-resources.db"));
        let host_operation_db_path = options
            .host_operation_db_path
            .or_else(|| environment_path(BROKER_OPEN_HOST_OPERATION_DB_ENV))
            .or_else(|| {
                host_state_dir
                    .as_ref()
                    .map(|directory| directory.join("host-operations.db"))
            });
        let worktree_root_override = options
            .worktree_root
            .or_else(|| environment_path(BROKER_OPEN_WORKTREE_ROOT_ENV));
        let worktree_root_environment_override = if worktree_root_override.is_none() {
            environment_path("AETHYME_WORKTREE_ROOT")
        } else {
            None
        };
        let collaboration_state_dir = options
            .collaboration_state_dir
            .or_else(|| environment_path(BROKER_OPEN_COLLABORATION_ROOT_ENV));
        let host_cache_dir = options
            .host_cache_dir
            .or_else(crate::host_state::default_host_cache_dir);

        let mut context = Self {
            host_state_dir_is_explicit: explicit_host_state_dir || inherited_explicit_host_state,
            host_state_dir,
            host_resource_db_path,
            host_cache_dir,
            host_operation_db_path,
            worktree_root_override,
            worktree_root_environment_override,
            collaboration_state_dir,
            preparation_cache_root_override,
            preparation_cache_root_identity,
            preparation_cache_root_state,
            preparation_cache_root_handle,
            capture_error: None,
            ..Self::default()
        };
        if let Err(reason) = context.anchor_relative_paths() {
            return Self::invalid(reason);
        }
        if let Err(error) = context.validate_preparation_cache_root_override() {
            return Self::invalid(error.to_string());
        }
        context
    }

    pub(crate) fn capture_for_repository(repo: &GitRepo) -> Result<Self, BrokerOpError> {
        let mut context = Self::capture(BrokerOpenOptions::default());
        context.ensure_valid()?;
        context.bind_repository(repo)?;
        Ok(context)
    }

    fn anchor_relative_paths(&mut self) -> Result<(), String> {
        let needs_current_directory = self
            .host_state_dir
            .as_deref()
            .is_some_and(|path| !path.is_absolute())
            || self
                .host_resource_db_path
                .as_deref()
                .is_some_and(|path| !path.is_absolute())
            || self
                .host_cache_dir
                .as_deref()
                .is_some_and(|path| !path.is_absolute())
            || self
                .host_operation_db_path
                .as_deref()
                .is_some_and(|path| !path.is_absolute())
            || self
                .worktree_root_override
                .as_deref()
                .is_some_and(|path| !path.is_absolute())
            || self
                .worktree_root_environment_override
                .as_deref()
                .is_some_and(|path| !path.is_absolute())
            || self
                .collaboration_state_dir
                .as_deref()
                .is_some_and(|path| !path.is_absolute());
        if !needs_current_directory {
            return Ok(());
        }
        let current_directory = std::env::current_dir().map_err(|error| {
            format!("cannot anchor relative storage paths to the parent current directory: {error}")
        })?;
        for path in [
            &mut self.host_state_dir,
            &mut self.host_resource_db_path,
            &mut self.host_cache_dir,
            &mut self.host_operation_db_path,
            &mut self.worktree_root_override,
            &mut self.worktree_root_environment_override,
            &mut self.collaboration_state_dir,
        ] {
            if let Some(path) = path
                && !path.is_absolute()
            {
                *path = current_directory.join(&*path);
            }
        }
        Ok(())
    }

    fn ensure_absolute_paths(&self) -> Result<(), String> {
        let repository_root = self
            .repository_root
            .as_deref()
            .ok_or_else(|| "child context has no repository root binding".to_string())?;
        if !repository_root.is_absolute() {
            return Err("child context repository root must be absolute".into());
        }
        let git_common_dir = self
            .git_common_dir
            .as_deref()
            .ok_or_else(|| "child context has no Git common-directory binding".to_string())?;
        if !git_common_dir.is_absolute() {
            return Err("child context Git common directory must be absolute".into());
        }
        for (name, path) in [
            ("host state directory", self.host_state_dir.as_deref()),
            (
                "host resource database",
                self.host_resource_db_path.as_deref(),
            ),
            ("host cache directory", self.host_cache_dir.as_deref()),
            (
                "host-operation database",
                self.host_operation_db_path.as_deref(),
            ),
            (
                "exact worktree root",
                self.worktree_root_override.as_deref(),
            ),
            (
                "worktree-root container",
                self.worktree_root_environment_override.as_deref(),
            ),
            (
                "collaboration state directory",
                self.collaboration_state_dir.as_deref(),
            ),
            (
                "preparation-cache root override",
                self.preparation_cache_root_override.as_deref(),
            ),
        ] {
            if let Some(path) = path
                && !path.is_absolute()
            {
                return Err(format!("child context {name} must be absolute"));
            }
        }
        Ok(())
    }

    fn bind_repository(&mut self, repo: &GitRepo) -> Result<(), BrokerOpError> {
        self.ensure_valid()?;
        let repository_root = std::fs::canonicalize(repo.root()).map_err(|source| {
            BrokerOpError::InvalidStorageContext {
                reason: format!(
                    "cannot canonicalize repository root {}: {source}",
                    repo.root().display()
                )
                .into_boxed_str(),
            }
        })?;
        let git_common_dir = repo.git_common_dir()?;
        let git_common_dir = std::fs::canonicalize(&git_common_dir).map_err(|source| {
            BrokerOpError::InvalidStorageContext {
                reason: format!(
                    "cannot canonicalize Git common directory {}: {source}",
                    git_common_dir.display()
                )
                .into_boxed_str(),
            }
        })?;
        Self::bind_identity_path(
            &mut self.repository_root,
            repository_root,
            "repository root",
        )?;
        Self::bind_identity_path(
            &mut self.git_common_dir,
            git_common_dir,
            "Git common directory",
        )
    }

    fn bind_identity_path(
        slot: &mut Option<PathBuf>,
        actual: PathBuf,
        label: &str,
    ) -> Result<(), BrokerOpError> {
        if let Some(expected) = slot {
            if expected != &actual {
                return Err(BrokerOpError::InvalidStorageContext {
                    reason: format!(
                        "child storage context is bound to a different {label}: {}",
                        expected.display()
                    )
                    .into_boxed_str(),
                });
            }
        } else {
            *slot = Some(actual);
        }
        Ok(())
    }

    fn anchor_override_path(&mut self, label: &str, path: PathBuf) -> PathBuf {
        if path.is_absolute() {
            return path;
        }
        match std::env::current_dir() {
            Ok(current_directory) => current_directory.join(path),
            Err(error) => {
                self.capture_error.get_or_insert_with(|| {
                    format!(
                        "cannot anchor relative {label} to the broker current directory: {error}"
                    )
                });
                path
            }
        }
    }

    pub(crate) fn repository_binding_refusal(
        &self,
        main_root: &Path,
        git_common_dir: Option<&Path>,
    ) -> Option<String> {
        if let Some(reason) = &self.capture_error {
            return Some(format!(
                "captured broker storage context is invalid: {reason}"
            ));
        }
        if self.repository_root.is_none() && self.git_common_dir.is_none() {
            return None;
        }
        let (Some(expected_root), Some(expected_common)) = (
            self.repository_root.as_deref(),
            self.git_common_dir.as_deref(),
        ) else {
            return Some(
                "captured broker storage context has an incomplete repository identity binding"
                    .into(),
            );
        };
        let actual_root = match std::fs::canonicalize(main_root) {
            Ok(root) => root,
            Err(error) => {
                return Some(format!(
                    "cannot verify captured storage context repository root {}: {error}",
                    main_root.display()
                ));
            }
        };
        let expected_root = match std::fs::canonicalize(expected_root) {
            Ok(root) => root,
            Err(error) => {
                return Some(format!(
                    "cannot verify captured storage context repository root {}: {error}",
                    expected_root.display()
                ));
            }
        };
        if expected_root != actual_root {
            return Some(format!(
                "captured broker storage context is bound to a different repository root: {}",
                expected_root.display()
            ));
        }
        let actual_common = match git_common_dir.map(Path::to_path_buf) {
            Some(path) => path,
            None => match GitRepo::discover(main_root).and_then(|repo| repo.git_common_dir()) {
                Ok(path) => path,
                Err(error) => {
                    return Some(format!(
                        "cannot verify captured storage context Git common directory: {error}"
                    ));
                }
            },
        };
        let actual_common = match std::fs::canonicalize(&actual_common) {
            Ok(path) => path,
            Err(error) => {
                return Some(format!(
                    "cannot verify captured storage context Git common directory {}: {error}",
                    actual_common.display()
                ));
            }
        };
        let expected_common = match std::fs::canonicalize(expected_common) {
            Ok(path) => path,
            Err(error) => {
                return Some(format!(
                    "cannot verify captured storage context Git common directory {}: {error}",
                    expected_common.display()
                ));
            }
        };
        if expected_common != actual_common {
            return Some(format!(
                "captured broker storage context is bound to a different Git common directory: {}",
                expected_common.display()
            ));
        }
        None
    }

    fn invalid(reason: impl Into<String>) -> Self {
        Self {
            capture_error: Some(reason.into()),
            ..Self::default()
        }
    }

    fn ensure_valid(&self) -> Result<(), BrokerOpError> {
        match &self.capture_error {
            Some(reason) => Err(BrokerOpError::InvalidStorageContext {
                reason: reason.clone().into_boxed_str(),
            }),
            None => Ok(()),
        }
    }

    fn initialize_preparation_cache_root(&mut self, repo: &GitRepo) -> Result<(), BrokerOpError> {
        let Some(root) = self.preparation_cache_root_override.clone() else {
            return Ok(());
        };
        self.validate_preparation_cache_root_override()?;
        if matches!(
            self.preparation_cache_root_state.as_ref(),
            Some(crate::broker::PreparationCacheRootState::Marked {
                cache_root_identity: Some(_),
                ..
            })
        ) {
            return self.validate_preparation_cache_root_override();
        }
        let root_identity = self
            .preparation_cache_root_identity
            .clone()
            .ok_or_else(|| BrokerOpError::InvalidStorageContext {
                reason: "preparation-cache root identity is unavailable".into(),
            })?;
        let root_handle = self
            .preparation_cache_root_handle
            .as_deref()
            .ok_or_else(|| BrokerOpError::InvalidStorageContext {
                reason: "preparation-cache root handle is unavailable".into(),
            })?;
        let resource_db = self
            .host_resource_db_path
            .as_deref()
            .ok_or_else(|| BrokerOpError::InvalidStorageContext {
                reason: "captured host-resource database is unavailable for preparation-cache initialization"
                    .into(),
            })?;
        let repository = crate::preparation::repository_identity_for_repo(repo);
        let request_id = preparation_root_request_id();
        let fingerprint = hex_digest(root.as_os_str().as_encoded_bytes());
        let request = crate::HostResourceRequest {
            schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: request_id.clone(),
            repository: repository.clone(),
            worktree_fingerprint: fingerprint,
            run_id: request_id,
            ttl_seconds: 30,
            holder_pid: Some(std::process::id()),
            resources: vec![crate::HostResourceRequirement {
                key: "preparation_cache".into(),
                resource: crate::HostResourceKind::ExclusiveKey {
                    name: crate::preparation::preparation_cache_lease_name(&repository),
                },
            }],
        };
        let mut coordinator = crate::HostResourceCoordinator::open(resource_db).map_err(|error| {
            BrokerOpError::InvalidStorageContext {
                reason: format!(
                    "cannot open captured host-resource DB for preparation-cache initialization: {error}"
                )
                .into_boxed_str(),
            }
        })?;
        let canonical_resource_db =
            canonical_coordinator_db_path(resource_db).map_err(|reason| {
                BrokerOpError::InvalidStorageContext {
                    reason: reason.into_boxed_str(),
                }
            })?;
        let grant = coordinator
            .acquire_with_wait(&request, Duration::ZERO, |_| {})
            .map_err(|error| BrokerOpError::InvalidStorageContext {
                reason: format!(
                    "cannot acquire the repository preparation-cache lease for root initialization: {error}"
                )
                .into_boxed_str(),
            })?;
        let initialize = crate::preparation::rotation_debt::initialize_preparation_cache_root(
            &root,
            &root_identity,
            root_handle,
            &canonical_resource_db,
            || preparation_cache_journal_has_rows(resource_db, &root),
        );
        let release = coordinator
            .release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
            .and_then(|released| {
                if released.lease_id == grant.lease.lease_id
                    && released.generation == grant.lease.generation
                    && released.state == crate::HostLeaseState::Released
                {
                    Ok(())
                } else {
                    Err(crate::HostResourceError::InvalidRequest(
                        "preparation-cache lease release returned a mismatched or unreleased lease"
                            .into(),
                    ))
                }
            });
        let initialized = match (initialize, release) {
            (Ok(initialized), Ok(())) => initialized,
            (Err(operation), Ok(())) => {
                return Err(BrokerOpError::InvalidStorageContext {
                    reason: format!("cannot initialize preparation-cache root: {operation}")
                        .into_boxed_str(),
                });
            }
            (Ok(_), Err(release)) => {
                return Err(BrokerOpError::InvalidStorageContext {
                    reason: format!(
                        "preparation-cache root initialization completed but its host-resource lease release is uncertain: {release}"
                    )
                    .into_boxed_str(),
                });
            }
            (Err(operation), Err(release)) => {
                return Err(BrokerOpError::InvalidStorageContext {
                    reason: format!(
                        "preparation-cache root initialization failed ({operation}) and its host-resource lease release is uncertain ({release})"
                    )
                    .into_boxed_str(),
                });
            }
        };
        if initialized.root_identity != root_identity {
            return Err(BrokerOpError::InvalidStorageContext {
                reason: "preparation-cache root identity changed during initialization".into(),
            });
        }
        self.preparation_cache_root_state =
            Some(crate::broker::PreparationCacheRootState::Marked {
                coordinator_db_path: initialized.coordinator_db_path.clone(),
                nonce: initialized.nonce,
                cache_root_identity: Some(initialized.cache_root_identity),
            });
        self.validate_preparation_cache_root_override()
    }

    fn validate_preparation_cache_root_override(&self) -> Result<(), BrokerOpError> {
        match (
            self.preparation_cache_root_override.as_deref(),
            self.preparation_cache_root_identity.as_deref(),
            self.preparation_cache_root_state.as_ref(),
            self.preparation_cache_root_handle.as_deref(),
        ) {
            (None, None, None, None) => Ok(()),
            (Some(root), Some(identity), Some(state), Some(directory)) => {
                validate_captured_preparation_cache_root(
                    root,
                    identity,
                    state,
                    self.host_resource_db_path.as_deref(),
                    directory,
                )
            }
            _ => Err(BrokerOpError::InvalidStorageContext {
                reason: "captured preparation-cache root state is incomplete".into(),
            }),
        }?;
        if let Some(crate::broker::PreparationCacheRootState::Marked {
            coordinator_db_path,
            ..
        }) = self.preparation_cache_root_state.as_ref()
        {
            let captured_resource_db = self.host_resource_db_path.as_deref().ok_or_else(|| {
                BrokerOpError::InvalidStorageContext {
                    reason: "marked preparation-cache root has no captured host-resource database"
                        .into(),
                }
            })?;
            let canonical =
                canonical_coordinator_db_path(captured_resource_db).map_err(|reason| {
                    BrokerOpError::InvalidStorageContext {
                        reason: reason.into_boxed_str(),
                    }
                })?;
            if &canonical != coordinator_db_path {
                return Err(BrokerOpError::InvalidStorageContext {
                    reason: "captured host-resource database differs from the preparation-cache root binding"
                        .into(),
                });
            }
        }
        Ok(())
    }

    pub(crate) fn preparation_cache_root_binding(
        &self,
    ) -> Result<Option<crate::broker::PreparationCacheRootBinding>, BrokerOpError> {
        self.ensure_valid()?;
        self.validate_preparation_cache_root_override()?;
        if let Some(root) = &self.preparation_cache_root_override {
            let (nonce, identity) = match self.preparation_cache_root_state.as_ref() {
                Some(crate::broker::PreparationCacheRootState::Marked {
                    coordinator_db_path: _,
                    nonce,
                    cache_root_identity: Some(identity),
                }) => (nonce.clone(), identity.clone()),
                Some(crate::broker::PreparationCacheRootState::Uninitialized)
                | Some(crate::broker::PreparationCacheRootState::Marked {
                    cache_root_identity: None,
                    ..
                }) => {
                    return Err(BrokerOpError::InvalidStorageContext {
                        reason: "preparation-cache root has not completed writable initialization"
                            .into(),
                    });
                }
                None => {
                    return Err(BrokerOpError::InvalidStorageContext {
                        reason: "preparation-cache root state is unavailable".into(),
                    });
                }
            };
            return Ok(Some(crate::broker::PreparationCacheRootBinding {
                path: preparation_cache_namespace_path(root, &nonce)?,
                identity: Some(identity),
                instance_nonce: Some(nonce),
                root_guard: Some(crate::broker::PreparationCacheRootGuard {
                    root_path: root.clone(),
                    root_identity: self.preparation_cache_root_identity.clone().ok_or_else(
                        || BrokerOpError::InvalidStorageContext {
                            reason: "captured preparation-cache root identity is unavailable"
                                .into(),
                        },
                    )?,
                    state: self.preparation_cache_root_state.clone().ok_or_else(|| {
                        BrokerOpError::InvalidStorageContext {
                            reason: "captured preparation-cache root state is unavailable".into(),
                        }
                    })?,
                    captured_resource_db_path: self.host_resource_db_path.clone().ok_or_else(
                        || BrokerOpError::InvalidStorageContext {
                            reason: "captured host-resource database is unavailable".into(),
                        },
                    )?,
                    root_handle: self.preparation_cache_root_handle.clone().ok_or_else(|| {
                        BrokerOpError::InvalidStorageContext {
                            reason: "captured preparation-cache root handle is unavailable".into(),
                        }
                    })?,
                    placement_root: None,
                }),
            }));
        }
        Ok(self
            .host_resource_db_path
            .as_deref()
            .and_then(Path::parent)
            .map(|parent| crate::broker::PreparationCacheRootBinding {
                path: parent.join("preparation-cache"),
                identity: None,
                instance_nonce: None,
                root_guard: None,
            }))
    }

    #[cfg(test)]
    pub(crate) fn preparation_cache_root(&self) -> Result<Option<PathBuf>, BrokerOpError> {
        Ok(self
            .preparation_cache_root_binding()?
            .map(|binding| binding.path))
    }

    pub(crate) fn gate_storage_locations(&self) -> crate::gates::GateStorageLocations {
        crate::gates::GateStorageLocations::Captured {
            host_resource_db_path: self.host_resource_db_path.clone(),
            host_cache_dir: self.host_cache_dir.clone(),
        }
    }

    fn collaboration_root(&self) -> Option<crate::collaboration_state::CollaborationRoot> {
        if let Some(path) = &self.collaboration_state_dir {
            return Some(crate::collaboration_state::CollaborationRoot::at_path(path));
        }
        if self.host_state_dir_is_explicit {
            return self
                .host_state_dir
                .as_deref()
                .map(crate::collaboration_state::CollaborationRoot::under_host_state);
        }
        None
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn preparation_cache_journal_has_rows(resource_db: &Path, root: &Path) -> std::io::Result<bool> {
    crate::preparation::rotation_journal::has_rows_at_or_below_root(resource_db, root)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn preparation_cache_journal_has_rows(_resource_db: &Path, _root: &Path) -> std::io::Result<bool> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "preparation-cache journal inspection is unsupported on this platform",
    ))
}

fn capture_preparation_cache_root(
    path: &Path,
) -> Result<
    (
        PathBuf,
        Vec<u8>,
        crate::broker::PreparationCacheRootState,
        std::sync::Arc<File>,
    ),
    String,
> {
    if !path.is_absolute() || path.to_str().is_none() {
        return Err("preparation-cache root override must be an absolute UTF-8 path".into());
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        format!(
            "cannot canonicalize preparation-cache root {}: {error}",
            path.display()
        )
    })?;
    if canonical != path {
        return Err("preparation-cache root override must already be canonical".into());
    }
    let capture = crate::preparation::rotation_debt::capture_preparation_cache_root(path).map_err(
        |error| {
            format!(
                "cannot capture preparation-cache root {}: {error}",
                path.display()
            )
        },
    )?;
    let state = match capture.state {
        crate::preparation::rotation_debt::PreparationCacheRootCaptureState::Uninitialized => {
            crate::broker::PreparationCacheRootState::Uninitialized
        }
        crate::preparation::rotation_debt::PreparationCacheRootCaptureState::Marked {
            coordinator_db_path,
            nonce,
            cache_root_identity,
        } => crate::broker::PreparationCacheRootState::Marked {
            coordinator_db_path,
            nonce,
            cache_root_identity,
        },
    };
    Ok((
        canonical,
        capture.identity,
        state,
        std::sync::Arc::new(capture.directory),
    ))
}

fn validate_captured_preparation_cache_root(
    path: &Path,
    expected_identity: &[u8],
    expected_state: &crate::broker::PreparationCacheRootState,
    captured_resource_db_path: Option<&Path>,
    pinned_directory: &File,
) -> Result<(), BrokerOpError> {
    crate::preparation::rotation_debt::validate_preparation_cache_root_handle(
        path,
        pinned_directory,
        expected_identity,
    )
    .map_err(|error| BrokerOpError::InvalidStorageContext {
        reason: format!(
            "captured preparation-cache root {} no longer matches its pinned identity: {error}",
            path.display()
        )
        .into_boxed_str(),
    })?;
    let capture = crate::preparation::rotation_debt::capture_preparation_cache_root(path).map_err(
        |error| BrokerOpError::InvalidStorageContext {
            reason: format!(
                "cannot recheck captured preparation-cache root {}: {error}",
                path.display()
            )
            .into_boxed_str(),
        },
    )?;
    if capture.identity != expected_identity
        || !matches_capture_state(expected_state, &capture.state)
    {
        return Err(BrokerOpError::InvalidStorageContext {
            reason: format!(
                "captured preparation-cache root {} has changed marker or namespace state",
                path.display()
            )
            .into_boxed_str(),
        });
    }
    if let crate::broker::PreparationCacheRootState::Marked {
        coordinator_db_path,
        ..
    } = expected_state
    {
        let captured_resource_db =
            captured_resource_db_path.ok_or_else(|| BrokerOpError::InvalidStorageContext {
                reason: "marked preparation-cache root has no captured host-resource database"
                    .into(),
            })?;
        let canonical = canonical_coordinator_db_path(captured_resource_db).map_err(|reason| {
            BrokerOpError::InvalidStorageContext {
                reason: reason.into_boxed_str(),
            }
        })?;
        if &canonical != coordinator_db_path {
            return Err(BrokerOpError::InvalidStorageContext {
                reason: "captured host-resource database differs from the preparation-cache root binding"
                    .into(),
            });
        }
    }
    Ok(())
}

pub(crate) fn validate_preparation_cache_root_guard(
    guard: &crate::broker::PreparationCacheRootGuard,
) -> Result<(), BrokerOpError> {
    if let Some(placement_root) = &guard.placement_root {
        if guard.root_path
            != placement_root
                .path
                .join(WORKTREE_VOLUME_PREPARATION_CACHE_ROOT)
        {
            return Err(BrokerOpError::InvalidStorageContext {
                reason: "worktree-volume cache root no longer matches its captured placement root"
                    .into(),
            });
        }
        validate_worktree_placement_root_guard(placement_root)?;
    }
    validate_captured_preparation_cache_root(
        &guard.root_path,
        &guard.root_identity,
        &guard.state,
        Some(&guard.captured_resource_db_path),
        &guard.root_handle,
    )
}

fn validate_worktree_placement_root_guard(
    guard: &crate::broker::PreparationCachePlacementRootGuard,
) -> Result<(), BrokerOpError> {
    if !matches!(
        std::fs::canonicalize(&guard.path),
        Ok(canonical) if canonical == guard.path
    ) {
        return Err(BrokerOpError::InvalidStorageContext {
            reason: format!(
                "captured worktree placement root {} is no longer canonical",
                guard.path.display()
            )
            .into_boxed_str(),
        });
    }
    let identity = crate::preparation::rotation_debt::DirectoryIdentity::capture(&guard.path)
        .and_then(|identity| identity.encode())
        .map_err(|error| BrokerOpError::InvalidStorageContext {
            reason: format!(
                "cannot recheck captured worktree placement root {}: {error}",
                guard.path.display()
            )
            .into_boxed_str(),
        })?;
    if identity != guard.identity {
        return Err(BrokerOpError::InvalidStorageContext {
            reason: format!(
                "captured worktree placement root {} was replaced",
                guard.path.display()
            )
            .into_boxed_str(),
        });
    }
    let marker = crate::worktree_registry::read_marker(&guard.path).map_err(|error| {
        BrokerOpError::InvalidStorageContext {
            reason: format!(
                "cannot recheck worktree placement ownership marker in {}: {error}",
                guard.path.display()
            )
            .into_boxed_str(),
        }
    })?;
    if marker != guard.marker {
        return Err(BrokerOpError::InvalidStorageContext {
            reason: format!(
                "worktree placement ownership marker changed in {}",
                guard.path.display()
            )
            .into_boxed_str(),
        });
    }
    Ok(())
}

fn capture_worktree_placement_root_guard(
    path: &Path,
    expected_marker: &WorktreeRootMarker,
) -> Result<crate::broker::PreparationCachePlacementRootGuard, BrokerOpError> {
    if !matches!(std::fs::canonicalize(path), Ok(canonical) if canonical == path) {
        return Err(BrokerOpError::InvalidStorageContext {
            reason: format!(
                "worktree placement root {} is not canonical",
                path.display()
            )
            .into_boxed_str(),
        });
    }
    let identity = crate::preparation::rotation_debt::DirectoryIdentity::capture(path)
        .and_then(|identity| identity.encode())
        .map_err(|error| BrokerOpError::InvalidStorageContext {
            reason: format!(
                "cannot capture worktree placement root {}: {error}",
                path.display()
            )
            .into_boxed_str(),
        })?;
    let marker = crate::worktree_registry::read_marker(path).map_err(|error| {
        BrokerOpError::InvalidStorageContext {
            reason: format!(
                "cannot verify worktree placement ownership marker in {}: {error}",
                path.display()
            )
            .into_boxed_str(),
        }
    })?;
    if &marker != expected_marker {
        return Err(BrokerOpError::InvalidStorageContext {
            reason: format!(
                "worktree placement root {} belongs to a different repository",
                path.display()
            )
            .into_boxed_str(),
        });
    }
    Ok(crate::broker::PreparationCachePlacementRootGuard {
        path: path.to_path_buf(),
        identity,
        marker,
    })
}

/// Capture an already initialized worktree-volume cache root for read-only
/// maintenance. A missing child is ordinary absence; an existing unmarked or
/// malformed child is a blocker and is never adopted here.
pub(crate) fn capture_worktree_volume_preparation_cache_root(
    placement_root: &Path,
    expected_marker: &WorktreeRootMarker,
    resource_db_path: &Path,
) -> Result<Option<crate::broker::PreparationCacheRootBinding>, BrokerOpError> {
    let root_path = placement_root.join(WORKTREE_VOLUME_PREPARATION_CACHE_ROOT);
    match std::fs::symlink_metadata(&root_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(BrokerOpError::InvalidStorageContext {
                reason: format!(
                    "cannot inspect worktree-volume preparation-cache root {}: {error}",
                    root_path.display()
                )
                .into_boxed_str(),
            });
        }
        Ok(_) => {}
    }
    let placement_guard = capture_worktree_placement_root_guard(placement_root, expected_marker)?;
    let (canonical, identity, state, directory) = capture_preparation_cache_root(&root_path)
        .map_err(|reason| BrokerOpError::InvalidStorageContext {
            reason: format!(
                "existing worktree-volume preparation-cache root {} is invalid: {reason}",
                root_path.display()
            )
            .into_boxed_str(),
        })?;
    let context = BrokerStorageContext {
        host_resource_db_path: Some(resource_db_path.to_path_buf()),
        preparation_cache_root_override: Some(canonical),
        preparation_cache_root_identity: Some(identity),
        preparation_cache_root_state: Some(state),
        preparation_cache_root_handle: Some(directory),
        ..BrokerStorageContext::default()
    };
    let mut binding = context
        .preparation_cache_root_binding()?
        .ok_or_else(|| BrokerOpError::InvalidStorageContext {
            reason: "existing worktree-volume preparation-cache root has no captured host-resource database".into(),
        })?;
    let placement_identity = crate::preparation::rotation_debt::DirectoryIdentity::capture(
        placement_root,
    )
    .map_err(|error| BrokerOpError::InvalidStorageContext {
        reason: format!(
            "cannot verify worktree-volume preparation-cache filesystem at {}: {error}",
            placement_root.display()
        )
        .into_boxed_str(),
    })?;
    let cache_identity = crate::preparation::rotation_debt::DirectoryIdentity::capture(
        &binding.path,
    )
    .map_err(|error| BrokerOpError::InvalidStorageContext {
        reason: format!(
            "cannot verify worktree-volume preparation-cache filesystem at {}: {error}",
            binding.path.display()
        )
        .into_boxed_str(),
    })?;
    if !placement_identity.same_filesystem(&cache_identity) {
        return Err(BrokerOpError::InvalidStorageContext {
            reason: "worktree-volume preparation-cache root is on a different filesystem from the worktree placement root".into(),
        });
    }
    let guard =
        binding
            .root_guard
            .as_mut()
            .ok_or_else(|| BrokerOpError::InvalidStorageContext {
                reason: "worktree-volume preparation-cache root has no retained root guard".into(),
            })?;
    guard.placement_root = Some(placement_guard);
    binding.validate()?;
    Ok(Some(binding))
}

fn matches_capture_state(
    expected: &crate::broker::PreparationCacheRootState,
    actual: &crate::preparation::rotation_debt::PreparationCacheRootCaptureState,
) -> bool {
    match (expected, actual) {
        (
            crate::broker::PreparationCacheRootState::Uninitialized,
            crate::preparation::rotation_debt::PreparationCacheRootCaptureState::Uninitialized,
        ) => true,
        (
            crate::broker::PreparationCacheRootState::Marked {
                coordinator_db_path: expected_db,
                nonce: expected_nonce,
                cache_root_identity: expected_identity,
            },
            crate::preparation::rotation_debt::PreparationCacheRootCaptureState::Marked {
                coordinator_db_path: actual_db,
                nonce: actual_nonce,
                cache_root_identity: actual_identity,
            },
        ) => {
            expected_db == actual_db
                && expected_nonce == actual_nonce
                && expected_identity == actual_identity
        }
        _ => false,
    }
}

fn canonical_coordinator_db_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.to_str().is_none() || path.as_os_str().len() > 4096 {
        return Err(
            "captured host-resource database path must be absolute, UTF-8, and bounded".into(),
        );
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        format!(
            "cannot resolve captured host-resource database {}: {error}",
            path.display()
        )
    })?;
    if !std::fs::metadata(&canonical)
        .map_err(|error| format!("cannot inspect captured host-resource database: {error}"))?
        .is_file()
    {
        return Err("captured host-resource database does not resolve to a regular file".into());
    }
    Ok(canonical)
}

fn preparation_cache_namespace_path(root: &Path, nonce: &str) -> Result<PathBuf, BrokerOpError> {
    if nonce.len() != 64
        || !nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(BrokerOpError::InvalidStorageContext {
            reason: "preparation-cache root nonce is malformed".into(),
        });
    }
    Ok(root.join(format!(".aethyme-preparation-cache-{nonce}")))
}

fn preparation_root_request_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("prep-cache-root-init-{}-{nanos}", std::process::id())
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn environment_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn set_command_path(command: &mut Command, name: &str, path: Option<&Path>) {
    if let Some(path) = path {
        command.env(name, path);
    } else {
        command.env_remove(name);
    }
}

/// The shared, read-only worktree-root choice used by both session placement
/// and disposable exact-tree verification. `repository_key` is already
/// appended to `preferred_root`; downstream storage must not add it again.
pub(crate) fn plan_worktree_root_for(
    main_root: &Path,
    git_common_dir: Option<&Path>,
    worktree_root_override: Option<&Path>,
) -> WorktreeRootPlan {
    let mut context = BrokerStorageContext::capture(BrokerOpenOptions::default());
    if let Some(root) = worktree_root_override {
        context.worktree_root_override = Some(root.to_path_buf());
        context.worktree_root_environment_override = None;
    }
    if let Some(reason) = context.repository_binding_refusal(main_root, git_common_dir) {
        return refused_worktree_root_plan(main_root, git_common_dir, reason);
    }
    plan_worktree_root_with_context(main_root, git_common_dir, &context)
}

fn refused_worktree_root_plan(
    main_root: &Path,
    git_common_dir: Option<&Path>,
    reason: String,
) -> WorktreeRootPlan {
    let repository_key = crate::host_state::repository_key(main_root, git_common_dir);
    WorktreeRootPlan {
        schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
        repository_root: main_root.to_path_buf(),
        repository_key,
        preferred_root: None,
        preferred_source: None,
        root_container: None,
        legacy_fallback_root: main_root.join(".aethyme/worktrees"),
        preferred_outside_repository: false,
        host_state_fallback_root: None,
        preferred_unavailable_reason: Some(reason.clone()),
        storage_context_refusal: Some(reason),
    }
}

pub(crate) fn plan_worktree_root_with_context(
    main_root: &Path,
    git_common_dir: Option<&Path>,
    context: &BrokerStorageContext,
) -> WorktreeRootPlan {
    let repository_key = crate::host_state::repository_key(main_root, git_common_dir);
    if let Some(reason) = &context.capture_error {
        return WorktreeRootPlan {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_root: main_root.to_path_buf(),
            repository_key,
            preferred_root: None,
            preferred_source: None,
            root_container: None,
            legacy_fallback_root: main_root.join(".aethyme/worktrees"),
            preferred_outside_repository: false,
            host_state_fallback_root: None,
            preferred_unavailable_reason: Some(format!(
                "captured broker storage context is invalid: {reason}"
            )),
            storage_context_refusal: Some(format!(
                "captured broker storage context is invalid: {reason}"
            )),
        };
    }
    let host_state_container = context
        .host_state_dir
        .clone()
        .filter(|_| {
            context.host_state_dir_is_explicit || !crate::host_state::path_is_ephemeral(main_root)
        })
        .map(|base| base.join("worktrees"));
    let mut preferred_unavailable_reason = None;
    let configured_root = match crate::worktree_location::WorktreeLocationConfig::load(main_root) {
        Ok(config) => config.and_then(|config| config.root.clone().map(|root| (root, config))),
        Err(error) => {
            preferred_unavailable_reason =
                Some(format!("[worktrees] configuration ignored: {error}"));
            None
        }
    };
    let mut host_state_fallback_root = None;
    let absolute_root = |path: &Path| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            main_root.join(path)
        }
    };
    let (preferred_root, preferred_source, root_container) =
        if let Some(root) = &context.worktree_root_override {
            (
                Some(absolute_root(root)),
                Some(WorktreeRootSource::LibraryOverride),
                None,
            )
        } else if let Some(base) = &context.worktree_root_environment_override {
            let container = absolute_root(base);
            (
                Some(container.join(&repository_key)),
                Some(WorktreeRootSource::EnvironmentOverride),
                Some(container),
            )
        } else if let Some((container, settings)) = configured_root {
            host_state_fallback_root = host_state_container
                .as_ref()
                .map(|container| container.join(&repository_key));
            preferred_unavailable_reason =
                configured_root_unavailable_for(main_root, &container, settings.min_free_bytes());
            (
                Some(container.join(&repository_key)),
                Some(WorktreeRootSource::RepositoryConfig),
                Some(container),
            )
        } else if let Some(container) = host_state_container {
            (
                Some(container.join(&repository_key)),
                Some(WorktreeRootSource::HostState),
                Some(container),
            )
        } else {
            (None, None, None)
        };
    let preferred_outside_repository = preferred_root.as_deref().is_some_and(|root| {
        root_path_is_inside_repository(main_root, root).is_ok_and(|inside| !inside)
    });
    WorktreeRootPlan {
        schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
        repository_root: main_root.to_path_buf(),
        repository_key,
        preferred_root,
        preferred_source,
        root_container,
        legacy_fallback_root: main_root.join(".aethyme/worktrees"),
        preferred_outside_repository,
        host_state_fallback_root,
        preferred_unavailable_reason,
        storage_context_refusal: None,
    }
}

/// Read-only root order used by both session preparation and exact-tree
/// verification. Configured roots may fall back through host state to the
/// repository; host state may fall back to the repository. Explicit roots are
/// strict: an unavailable environment or library override is an error, not a
/// reason to put work on a different volume.
pub(crate) fn worktree_root_candidates_for(
    plan: &WorktreeRootPlan,
) -> Vec<(PathBuf, WorktreeRootSource)> {
    if plan.storage_context_refusal.is_some() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    match plan.preferred_source {
        Some(WorktreeRootSource::RepositoryConfig) => {
            if plan.preferred_unavailable_reason.is_none()
                && let Some(root) = &plan.preferred_root
            {
                candidates.push((root.clone(), WorktreeRootSource::RepositoryConfig));
            }
            if let Some(root) = &plan.host_state_fallback_root {
                candidates.push((root.clone(), WorktreeRootSource::HostState));
            }
            candidates.push((
                plan.legacy_fallback_root.clone(),
                WorktreeRootSource::RepositoryFallback,
            ));
        }
        Some(WorktreeRootSource::HostState) => {
            if let Some(root) = &plan.preferred_root {
                candidates.push((root.clone(), WorktreeRootSource::HostState));
            }
            candidates.push((
                plan.legacy_fallback_root.clone(),
                WorktreeRootSource::RepositoryFallback,
            ));
        }
        Some(
            source
            @ (WorktreeRootSource::EnvironmentOverride | WorktreeRootSource::LibraryOverride),
        ) => {
            if let Some(root) = &plan.preferred_root {
                candidates.push((root.clone(), source));
            }
        }
        Some(WorktreeRootSource::RepositoryFallback) | None => {
            candidates.push((
                plan.legacy_fallback_root.clone(),
                WorktreeRootSource::RepositoryFallback,
            ));
        }
    }
    candidates
}

fn configured_root_unavailable_for(
    main_root: &Path,
    base: &Path,
    min_free_bytes: u64,
) -> Option<String> {
    if let Some(reason) = crate::worktree_location::base_unavailable_reason(base) {
        return Some(reason);
    }
    match root_path_is_inside_repository(main_root, base) {
        Ok(true) => {
            return Some(format!(
                "{} resolves inside repository {}",
                base.display(),
                main_root.display()
            ));
        }
        Ok(false) => {}
        Err(reason) => return Some(reason),
    }
    match crate::disk_headroom::available_bytes_for(main_root, base) {
        Some(free) if free < min_free_bytes => Some(format!(
            "{} has {} free, below the {} required (worktrees.min_free_bytes)",
            base.display(),
            crate::disk_headroom::format_gibibytes(free),
            crate::disk_headroom::format_gibibytes(min_free_bytes)
        )),
        _ => None,
    }
}

fn root_path_is_inside_repository(main_root: &Path, path: &Path) -> Result<bool, String> {
    let repository = canonical_prefix(main_root)?;
    let candidate = canonical_prefix(path)?;
    Ok(candidate.starts_with(repository))
}

/// Resolve the nearest existing ancestor, then walk the missing suffix while
/// resolving every component that exists. A `..` after a missing component is
/// ambiguous: it can skip the directory `create_dir_all` would first create,
/// or re-enter an existing symlink. Refuse it before any root is created.
pub(crate) fn canonical_prefix(path: &Path) -> Result<PathBuf, String> {
    for ancestor in path.ancestors() {
        let Ok(mut resolved) = ancestor.canonicalize() else {
            continue;
        };
        let remainder = path.strip_prefix(ancestor).unwrap_or(Path::new(""));
        let mut unresolved_component = false;
        for component in remainder.components() {
            match component {
                std::path::Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
                std::path::Component::RootDir => {
                    resolved = PathBuf::from(std::path::MAIN_SEPARATOR.to_string());
                    unresolved_component = false;
                }
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    if unresolved_component {
                        return Err(format!(
                            "{} contains `..` after a missing path component; refusing ambiguous path resolution",
                            path.display()
                        ));
                    }
                    resolved.pop();
                }
                std::path::Component::Normal(name) => {
                    let candidate = resolved.join(name);
                    match candidate.canonicalize() {
                        Ok(canonical) => {
                            resolved = canonical;
                            unresolved_component = false;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            resolved = candidate;
                            unresolved_component = true;
                        }
                        Err(error) => {
                            return Err(format!(
                                "cannot resolve path component {}: {error}",
                                candidate.display()
                            ));
                        }
                    }
                }
            }
        }
        return Ok(resolved);
    }
    Err(format!(
        "no existing ancestor can be resolved for {}",
        path.display()
    ))
}

fn preflight_worktree_root(
    main_root: &Path,
    path: &Path,
    source: WorktreeRootSource,
    require_external: bool,
    min_free_bytes: Option<u64>,
    mut removable_volume_refusal: impl FnMut(&Path) -> Option<String>,
    mut available_bytes: impl FnMut(&Path) -> Option<u64>,
) -> Result<(), String> {
    if let Some(reason) = removable_volume_refusal(path) {
        return Err(reason);
    }
    if let Some(ancestor) = path.ancestors().find(|ancestor| ancestor.exists())
        && !ancestor.is_dir()
    {
        return Err(format!(
            "nearest existing ancestor {} is not a directory",
            ancestor.display()
        ));
    }
    if require_external {
        match root_path_is_inside_repository(main_root, path) {
            Ok(true) => {
                return Err(format!(
                    "{} must resolve outside repository {}; choose an external AETHYME_WORKTREE_ROOT",
                    source.as_str(),
                    main_root.display()
                ));
            }
            Ok(false) => {}
            Err(reason) => {
                return Err(format!(
                    "cannot safely resolve {} before creating it: {reason}",
                    path.display()
                ));
            }
        }
    }
    // Placement overrides select a location; they do not opt out of the
    // recovery reserve. Only the configured root uses its explicit policy.
    let minimum = worktree_storage_minimum(source, min_free_bytes);
    if minimum > 0 {
        let free = available_bytes(path).ok_or_else(|| {
            format!(
                "cannot inspect free space at {}; refusing checkout allocation until its volume can be inspected",
                path.display()
            )
        })?;
        if free < minimum {
            let policy = if source == WorktreeRootSource::RepositoryConfig {
                "worktrees.min_free_bytes"
            } else {
                "broker recovery reserve"
            };
            return Err(format!(
                "{} has {} free, below the {} required ({policy})",
                path.display(),
                crate::disk_headroom::format_gibibytes(free),
                crate::disk_headroom::format_gibibytes(minimum)
            ));
        }
    }
    Ok(())
}

fn worktree_storage_minimum(source: WorktreeRootSource, configured: Option<u64>) -> u64 {
    if source == WorktreeRootSource::RepositoryConfig {
        configured.unwrap_or(crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES)
    } else {
        crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES
    }
}

#[cfg(test)]
fn prepare_worktree_root_directory(
    main_root: &Path,
    path: &Path,
    source: WorktreeRootSource,
    require_external: bool,
    min_free_bytes: Option<u64>,
    removable_volume_refusal: impl FnMut(&Path) -> Option<String>,
    available_bytes: impl FnMut(&Path) -> Option<u64>,
) -> Result<PathBuf, String> {
    preflight_worktree_root(
        main_root,
        path,
        source,
        require_external,
        min_free_bytes,
        removable_volume_refusal,
        available_bytes,
    )?;
    std::fs::create_dir_all(path).map_err(|error| error.to_string())?;
    path.canonicalize().map_err(|error| error.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BoundDirectoryIdentity {
    canonical_path: PathBuf,
    device: u64,
    inode: u64,
    uid: u32,
}

struct FreshWorktreeRootCandidate {
    root: PathBuf,
    parent_identity_json: String,
    layout: crate::store::repository_binding::RepositoryWorktreeRootLayout,
    source: WorktreeRootSource,
}

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FreshRootRecoveryPathState {
    Missing,
    Empty,
    ExactMarker,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FreshRootRecoveryCargoState {
    EmptyDirectory,
    ExactConfig,
    ExactWriterTemporary { name: String },
    ExactWriterConfigAndTemporary { name: String },
    ExactAtomicTemporary { name: String },
    ExactAtomicConfigAndTemporary { name: String },
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
struct FreshRootRecoveryCargoInventory {
    directory_identity: crate::worktree_registry::RecoveryArtifactIdentity,
    state: FreshRootRecoveryCargoState,
    file_identity: Option<crate::worktree_registry::RecoveryArtifactIdentity>,
    bytes_sha256: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
struct FreshRootRecoveryInventory {
    state: FreshRootRecoveryPathState,
    root_identity: Option<BoundDirectoryIdentity>,
    root_mode: Option<u32>,
    entries: Vec<String>,
    marker_bytes_sha256: Option<String>,
    marker_identity: Option<crate::worktree_registry::RecoveryArtifactIdentity>,
    cargo: Option<FreshRootRecoveryCargoInventory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FreshRootRecoveryApplyCheckpoint {
    BeforeRegistryInitialization,
    AfterRegistryInitializationBeforeMarker,
    MarkerDurablyPublishedBeforeRegistryRecord,
    RegistryRowDurablyRecordedBeforeBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FreshRootPublicationCheckpoint {
    MarkerDurablyPublishedBeforeRegistryRecord,
    RegistryRowDurablyRecorded,
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct FreshRootRecoveryPlan {
    pub(crate) schema_version: u32,
    pub(crate) binding: crate::store::repository_binding::RepositoryWorktreeBinding,
    pub(crate) repository_root: PathBuf,
    pub(crate) repository_key: String,
    pub(crate) broker_database: PathBuf,
    pub(crate) host_operation_database: PathBuf,
    pub(crate) worktree_root: PathBuf,
    pub(crate) layout: crate::store::repository_binding::RepositoryWorktreeRootLayout,
    pub(crate) source: WorktreeRootSource,
    pub(crate) parent_identity: BoundDirectoryIdentity,
    root_inventory: FreshRootRecoveryInventory,
    pub(crate) registry: crate::worktree_registry::FreshRootRegistryObservation,
    pub(crate) expected_marker_bytes_sha256: String,
    pub(crate) binding_record_sha256: String,
    pub(crate) can_apply: bool,
    pub(crate) refusal: Option<String>,
    pub(crate) digest: String,
}

#[derive(serde::Serialize)]
struct FreshRootRecoveryDigestInput<'a> {
    schema_version: u32,
    binding: &'a crate::store::repository_binding::RepositoryWorktreeBinding,
    repository_root: &'a Path,
    repository_key: &'a str,
    broker_database: &'a Path,
    host_operation_database: &'a Path,
    worktree_root: &'a Path,
    layout: crate::store::repository_binding::RepositoryWorktreeRootLayout,
    source: WorktreeRootSource,
    parent_identity: &'a BoundDirectoryIdentity,
    root_inventory: &'a FreshRootRecoveryInventory,
    registry: &'a crate::worktree_registry::FreshRootRegistryObservation,
    expected_marker_bytes_sha256: &'a str,
    binding_record_sha256: &'a str,
    can_apply: bool,
    refusal: &'a Option<String>,
}

fn fresh_root_recovery_marker_bytes(marker: &WorktreeRootMarker) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec_pretty(marker).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if bytes.len() > crate::worktree_registry::MAX_RECORD_BYTES {
        return Err("ownership marker exceeds its byte limit".into());
    }
    Ok(bytes)
}

#[cfg(unix)]
fn recovery_path_file_bytes(
    path: &Path,
) -> Result<(Vec<u8>, crate::worktree_registry::RecoveryArtifactIdentity), String> {
    recovery_path_file_bytes_with_links(path, 1)
}

#[cfg(unix)]
fn recovery_path_file_bytes_with_links(
    path: &Path,
    expected_links: u64,
) -> Result<(Vec<u8>, crate::worktree_registry::RecoveryArtifactIdentity), String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let before = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    // SAFETY: geteuid has no pointer arguments or caller-side memory preconditions.
    let effective_uid = unsafe { libc::geteuid() };
    if before.file_type().is_symlink()
        || !before.is_file()
        || before.nlink() != expected_links
        || before.uid() != effective_uid
        || before.len() > crate::worktree_registry::MAX_RECORD_BYTES as u64
    {
        return Err(format!(
            "{} is not a bounded regular recovery file with {expected_links} link(s)",
            path.display()
        ));
    }
    let identity = crate::worktree_registry::RecoveryArtifactIdentity {
        device: before.dev(),
        inode: before.ino(),
        owner: before.uid(),
        mode: before.mode() & 0o7777,
        links: before.nlink(),
        bytes: before.len(),
    };
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let file = options.open(path).map_err(|error| error.to_string())?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    let opened_identity = crate::worktree_registry::RecoveryArtifactIdentity {
        device: opened.dev(),
        inode: opened.ino(),
        owner: opened.uid(),
        mode: opened.mode() & 0o7777,
        links: opened.nlink(),
        bytes: opened.len(),
    };
    if opened_identity != identity {
        return Err(format!("{} changed while opening", path.display()));
    }
    let mut bytes = Vec::with_capacity(identity.bytes as usize);
    file.take(crate::worktree_registry::MAX_RECORD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let after = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    let after_identity = crate::worktree_registry::RecoveryArtifactIdentity {
        device: after.dev(),
        inode: after.ino(),
        owner: after.uid(),
        mode: after.mode() & 0o7777,
        links: after.nlink(),
        bytes: after.len(),
    };
    if after_identity != identity || bytes.len() as u64 != identity.bytes {
        return Err(format!("{} changed while reading", path.display()));
    }
    Ok((bytes, identity))
}

#[cfg(not(unix))]
fn recovery_path_file_bytes(
    _path: &Path,
) -> Result<(Vec<u8>, crate::worktree_registry::RecoveryArtifactIdentity), String> {
    Err("fresh-root recovery requires Unix no-follow file checks".into())
}

#[cfg(unix)]
fn fresh_root_recovery_inventory(
    root: &Path,
    expected_marker: &WorktreeRootMarker,
) -> Result<FreshRootRecoveryInventory, String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if canonical_prefix(root)? != root {
        return Err("reserved root path is not its canonical unresolved name".into());
    }
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FreshRootRecoveryInventory {
                state: FreshRootRecoveryPathState::Missing,
                root_identity: None,
                root_mode: None,
                entries: Vec::new(),
                marker_bytes_sha256: None,
                marker_identity: None,
                cargo: None,
            });
        }
        Err(error) => return Err(error.to_string()),
    };
    // SAFETY: geteuid has no pointer arguments or caller-side memory preconditions.
    let effective_uid = unsafe { libc::geteuid() };
    if root_metadata.file_type().is_symlink()
        || !root_metadata.is_dir()
        || root_metadata.uid() != effective_uid
    {
        return Err("reserved root is not a real directory owned by the current user".into());
    }
    let root_identity = bound_directory_identity(root)?;
    verify_private_worktree_root_permissions(root)?;
    let root_mode = root_metadata.permissions().mode() & 0o7777;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|error| error.to_string())? {
        if entries.len() >= 4 {
            return Err("reserved root exceeds the recovery entry limit".into());
        }
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "reserved root contains a non-UTF-8 entry".to_owned())?;
        if !matches!(
            name.as_str(),
            crate::broker::WORKTREE_ROOT_MARKER | ".cargo"
        ) {
            return Err(format!("unexpected reserved-root entry {name:?}"));
        }
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if kind.is_symlink()
            || (name == ".cargo" && !kind.is_dir())
            || (name != ".cargo" && !kind.is_file())
        {
            return Err(format!(
                "reserved-root entry {name:?} has an unexpected type"
            ));
        }
        entries.push(name);
    }
    entries.sort();
    let marker_path = root.join(crate::broker::WORKTREE_ROOT_MARKER);
    let (marker_bytes_sha256, marker_identity) = if entries
        .iter()
        .any(|name| name == crate::broker::WORKTREE_ROOT_MARKER)
    {
        let (bytes, identity) = recovery_path_file_bytes(&marker_path)?;
        let expected_bytes = fresh_root_recovery_marker_bytes(expected_marker)?;
        if bytes != expected_bytes {
            return Err(
                "reserved-root marker bytes differ from the exact fresh reservation".into(),
            );
        }
        (Some(hex_digest(&bytes)), Some(identity))
    } else {
        (None, None)
    };
    let cargo = if entries.iter().any(|name| name == ".cargo") {
        Some(fresh_root_recovery_cargo_inventory(&root.join(".cargo"))?)
    } else {
        None
    };
    if bound_directory_identity(root)? != root_identity {
        return Err("reserved root identity changed during inspection".into());
    }
    let state = if marker_bytes_sha256.is_some() {
        FreshRootRecoveryPathState::ExactMarker
    } else if entries.is_empty() {
        FreshRootRecoveryPathState::Empty
    } else {
        return Err("unmarked reserved root is not empty".into());
    };
    Ok(FreshRootRecoveryInventory {
        state,
        root_identity: Some(root_identity),
        root_mode: Some(root_mode),
        entries,
        marker_bytes_sha256,
        marker_identity,
        cargo,
    })
}

#[cfg(not(unix))]
fn fresh_root_recovery_inventory(
    _root: &Path,
    _expected_marker: &WorktreeRootMarker,
) -> Result<FreshRootRecoveryInventory, String> {
    Err("fresh-root recovery requires Unix no-follow filesystem checks".into())
}

#[cfg(unix)]
fn fresh_root_recovery_cargo_temporary_name(name: &str) -> bool {
    // tempfile 3.27.0 uses six random ASCII alphanumeric characters by default.
    name.strip_prefix(".config.toml-").is_some_and(|suffix| {
        suffix.len() == 6 && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}

#[cfg(unix)]
fn fresh_root_recovery_cargo_writer_temporary_name(name: &str) -> bool {
    let Some(suffix) = name
        .strip_prefix(".config.")
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    let mut parts = suffix.split('.');
    let pid = parts.next().unwrap_or_default();
    let timestamp = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || pid.is_empty()
        || !pid.bytes().all(|byte| byte.is_ascii_digit())
        || timestamp.is_empty()
        || !timestamp.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    let (Ok(pid_value), Ok(timestamp_value)) = (pid.parse::<u32>(), timestamp.parse::<u64>())
    else {
        return false;
    };
    pid_value > 0
        && timestamp_value > 0
        && pid == pid_value.to_string()
        && timestamp == timestamp_value.to_string()
}

#[cfg(unix)]
fn recovery_directory_artifact_identity(
    metadata: &std::fs::Metadata,
) -> crate::worktree_registry::RecoveryArtifactIdentity {
    use std::os::unix::fs::MetadataExt;

    crate::worktree_registry::RecoveryArtifactIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        owner: metadata.uid(),
        mode: metadata.mode() & 0o7777,
        links: metadata.nlink(),
        bytes: metadata.len(),
    }
}

#[cfg(unix)]
fn same_recovery_directory_object(
    left: &crate::worktree_registry::RecoveryArtifactIdentity,
    right: &crate::worktree_registry::RecoveryArtifactIdentity,
) -> bool {
    left.device == right.device
        && left.inode == right.inode
        && left.owner == right.owner
        && left.mode == right.mode
        && left.links == right.links
}

#[cfg(unix)]
fn fresh_root_recovery_cargo_inventory(
    directory: &Path,
) -> Result<FreshRootRecoveryCargoInventory, String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
    // SAFETY: geteuid has no pointer arguments or caller-side memory preconditions.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != effective_uid
        || metadata.mode() & 0o022 != 0
    {
        return Err("reserved root .cargo is not a private real directory".into());
    }
    let directory_identity = recovery_directory_artifact_identity(&metadata);
    let mut entries = Vec::<(String, PathBuf)>::new();
    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        if entries.len() >= 2 {
            return Err("reserved root .cargo exceeds the recovery entry limit".into());
        }
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "reserved root .cargo contains a non-UTF-8 entry".to_owned())?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_symlink() || !file_type.is_file() {
            return Err("reserved root .cargo config is not a regular file".into());
        }
        entries.push((name, entry.path()));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    if entries.is_empty() {
        let directory_after =
            std::fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
        let identity_after = recovery_directory_artifact_identity(&directory_after);
        if identity_after != directory_identity {
            return Err("reserved root .cargo changed during inspection".into());
        }
        return Ok(FreshRootRecoveryCargoInventory {
            directory_identity,
            state: FreshRootRecoveryCargoState::EmptyDirectory,
            file_identity: None,
            bytes_sha256: None,
        });
    }

    let expected_bytes = crate::broker::WORKTREE_CARGO_CONFIG.as_bytes();
    let (state, file_identity, bytes) = if entries.len() == 1 {
        let (name, path) = &entries[0];
        let (bytes, identity) = recovery_path_file_bytes(path)?;
        if bytes != expected_bytes {
            return Err("reserved root .cargo bytes differ from the exact broker defaults".into());
        }
        let state = if name == "config.toml" {
            FreshRootRecoveryCargoState::ExactConfig
        } else if fresh_root_recovery_cargo_temporary_name(name) {
            if identity.mode != 0o600 {
                return Err("reserved root .cargo atomic temporary is not mode 0600".into());
            }
            FreshRootRecoveryCargoState::ExactAtomicTemporary { name: name.clone() }
        } else if fresh_root_recovery_cargo_writer_temporary_name(name) {
            if identity.mode & 0o022 != 0 {
                return Err("reserved root .cargo writer temporary is group/world writable".into());
            }
            FreshRootRecoveryCargoState::ExactWriterTemporary { name: name.clone() }
        } else {
            return Err(format!("unexpected reserved root .cargo entry {name:?}"));
        };
        (state, identity, bytes)
    } else {
        let config = entries
            .iter()
            .find(|(name, _)| name == "config.toml")
            .ok_or_else(|| "reserved root .cargo has an unsupported two-file prefix".to_owned())?;
        let (temporary_name, temporary_path) = entries
            .iter()
            .find(|(name, _)| {
                fresh_root_recovery_cargo_temporary_name(name)
                    || fresh_root_recovery_cargo_writer_temporary_name(name)
            })
            .ok_or_else(|| "reserved root .cargo has an unsupported two-file prefix".to_owned())?;
        let (config_bytes, config_identity) = recovery_path_file_bytes_with_links(&config.1, 2)?;
        let (temporary_bytes, temporary_identity) =
            recovery_path_file_bytes_with_links(temporary_path, 2)?;
        let legacy_writer = fresh_root_recovery_cargo_writer_temporary_name(temporary_name);
        let mode_is_safe = if legacy_writer {
            config_identity.mode & 0o022 == 0
        } else {
            config_identity.mode == 0o600
        };
        if config_bytes != expected_bytes
            || temporary_bytes != expected_bytes
            || config_identity != temporary_identity
            || !mode_is_safe
        {
            return Err(
                "reserved root .cargo publication pair is not the exact same private inode".into(),
            );
        }
        let state = if fresh_root_recovery_cargo_writer_temporary_name(temporary_name) {
            FreshRootRecoveryCargoState::ExactWriterConfigAndTemporary {
                name: temporary_name.clone(),
            }
        } else {
            FreshRootRecoveryCargoState::ExactAtomicConfigAndTemporary {
                name: temporary_name.clone(),
            }
        };
        (state, config_identity, config_bytes)
    };

    let directory_after =
        std::fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
    let directory_identity_after = recovery_directory_artifact_identity(&directory_after);
    if directory_identity_after != directory_identity {
        return Err("reserved root .cargo changed during inspection".into());
    }
    Ok(FreshRootRecoveryCargoInventory {
        directory_identity,
        state,
        file_identity: Some(file_identity),
        bytes_sha256: Some(hex_digest(&bytes)),
    })
}

#[cfg(unix)]
fn sync_fresh_root_recovery_cargo_directory(
    directory: &Path,
    expected: &crate::worktree_registry::RecoveryArtifactIdentity,
) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;

    let before = std::fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
    if before.file_type().is_symlink()
        || !before.is_dir()
        || &recovery_directory_artifact_identity(&before) != expected
    {
        return Err("reserved root .cargo changed before syncing its entries".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let handle = options.open(directory).map_err(|error| error.to_string())?;
    if !handle
        .metadata()
        .map(|metadata| recovery_directory_artifact_identity(&metadata) == *expected)
        .unwrap_or(false)
    {
        return Err("opened .cargo directory identity differs from the reviewed identity".into());
    }
    handle.sync_all().map_err(|error| error.to_string())?;
    let after = std::fs::symlink_metadata(directory).map_err(|error| error.to_string())?;
    if after.file_type().is_symlink()
        || !after.is_dir()
        || &recovery_directory_artifact_identity(&after) != expected
    {
        return Err("reserved root .cargo changed while syncing its entries".into());
    }
    Ok(())
}

#[cfg(unix)]
fn sync_fresh_root_recovery_cargo_file(
    directory: &Path,
    name: &str,
    expected: &crate::worktree_registry::RecoveryArtifactIdentity,
    expected_links: u64,
) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let path = directory.join(name);
    let (bytes_before, identity_before) =
        recovery_path_file_bytes_with_links(&path, expected_links)?;
    if &identity_before != expected
        || bytes_before != crate::broker::WORKTREE_CARGO_CONFIG.as_bytes()
    {
        return Err("reserved root .cargo file changed before its data sync".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let file = options.open(&path).map_err(|error| error.to_string())?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    let opened_identity = crate::worktree_registry::RecoveryArtifactIdentity {
        device: opened.dev(),
        inode: opened.ino(),
        owner: opened.uid(),
        mode: opened.mode() & 0o7777,
        links: opened.nlink(),
        bytes: opened.len(),
    };
    if !opened.is_file() || &opened_identity != expected {
        return Err("opened .cargo file identity differs from the reviewed identity".into());
    }
    file.sync_all()
        .map_err(|error| format!("cannot durably sync reserved root .cargo file: {error}"))?;
    let (bytes_after, identity_after) = recovery_path_file_bytes_with_links(&path, expected_links)?;
    if bytes_after != bytes_before || &identity_after != expected {
        return Err("reserved root .cargo file changed during its data sync".into());
    }
    Ok(())
}

#[cfg(unix)]
fn normalize_fresh_root_recovery_cargo(
    root: &Path,
    expected_root: &BoundDirectoryIdentity,
    expected: &FreshRootRecoveryCargoInventory,
) -> Result<(), String> {
    require_bound_directory_identity(root, expected_root)?;
    let directory = root.join(".cargo");
    let current = fresh_root_recovery_cargo_inventory(&directory)?;
    if &current != expected {
        return Err("reserved root .cargo changed after recovery-plan review".into());
    }

    let (temporary_name, is_temporary_only, is_legacy_writer) = match &current.state {
        FreshRootRecoveryCargoState::EmptyDirectory => return Ok(()),
        FreshRootRecoveryCargoState::ExactConfig => {
            let file_identity = current
                .file_identity
                .as_ref()
                .ok_or_else(|| "reviewed Cargo config has no file identity".to_owned())?;
            sync_fresh_root_recovery_cargo_file(&directory, "config.toml", file_identity, 1)?;
            sync_fresh_root_recovery_cargo_directory(&directory, &current.directory_identity)?;
            if fresh_root_recovery_cargo_inventory(&directory)? != current {
                return Err(
                    "reserved root .cargo changed after its file and directory sync".into(),
                );
            }
            require_bound_directory_identity(root, expected_root)?;
            return Ok(());
        }
        FreshRootRecoveryCargoState::ExactWriterTemporary { name }
        | FreshRootRecoveryCargoState::ExactAtomicTemporary { name } => {
            let legacy = matches!(
                &current.state,
                FreshRootRecoveryCargoState::ExactWriterTemporary { .. }
            );
            let file_identity = current
                .file_identity
                .as_ref()
                .ok_or_else(|| "reviewed Cargo temporary has no file identity".to_owned())?;
            sync_fresh_root_recovery_cargo_file(&directory, name, file_identity, 1)?;
            (name.as_str(), true, legacy)
        }
        FreshRootRecoveryCargoState::ExactWriterConfigAndTemporary { name }
        | FreshRootRecoveryCargoState::ExactAtomicConfigAndTemporary { name } => {
            let legacy = matches!(
                &current.state,
                FreshRootRecoveryCargoState::ExactWriterConfigAndTemporary { .. }
            );
            (name.as_str(), false, legacy)
        }
    };

    if is_temporary_only {
        let temporary = directory.join(temporary_name);
        let config = directory.join("config.toml");
        std::fs::hard_link(&temporary, &config).map_err(|error| {
            format!("cannot publish exact recovery config without replacement: {error}")
        })?;
        require_bound_directory_identity(root, expected_root)?;
    }

    let linked = fresh_root_recovery_cargo_inventory(&directory)?;
    let expected_pair_state = if is_legacy_writer {
        FreshRootRecoveryCargoState::ExactWriterConfigAndTemporary {
            name: temporary_name.to_owned(),
        }
    } else {
        FreshRootRecoveryCargoState::ExactAtomicConfigAndTemporary {
            name: temporary_name.to_owned(),
        }
    };
    let mut linked_file = current
        .file_identity
        .clone()
        .ok_or_else(|| "reviewed Cargo publication has no file identity".to_owned())?;
    linked_file.links = 2;
    if linked.state != expected_pair_state
        || linked.file_identity.as_ref() != Some(&linked_file)
        || linked.bytes_sha256 != current.bytes_sha256
        || !same_recovery_directory_object(&linked.directory_identity, &current.directory_identity)
    {
        return Err("Cargo config publication pair differs from the reviewed inode".into());
    }
    sync_fresh_root_recovery_cargo_file(&directory, "config.toml", &linked_file, 2)?;
    sync_fresh_root_recovery_cargo_directory(&directory, &linked.directory_identity)?;
    if fresh_root_recovery_cargo_inventory(&directory)? != linked {
        return Err("reserved root .cargo pair changed after its data and directory sync".into());
    }

    std::fs::remove_file(directory.join(temporary_name))
        .map_err(|error| format!("cannot remove exact recovery temporary link: {error}"))?;
    require_bound_directory_identity(root, expected_root)?;
    let final_inventory = fresh_root_recovery_cargo_inventory(&directory)?;
    let mut final_file = linked_file;
    final_file.links = 1;
    if final_inventory.state != FreshRootRecoveryCargoState::ExactConfig
        || final_inventory.file_identity.as_ref() != Some(&final_file)
        || final_inventory.bytes_sha256 != current.bytes_sha256
        || !same_recovery_directory_object(
            &final_inventory.directory_identity,
            &linked.directory_identity,
        )
    {
        return Err("recovered Cargo defaults do not match the reviewed config inode".into());
    }
    sync_fresh_root_recovery_cargo_directory(&directory, &final_inventory.directory_identity)?;
    if fresh_root_recovery_cargo_inventory(&directory)? != final_inventory {
        return Err("recovered Cargo defaults changed after directory sync".into());
    }
    require_bound_directory_identity(root, expected_root)?;
    Ok(())
}

#[cfg(not(unix))]
fn normalize_fresh_root_recovery_cargo(
    _root: &Path,
    _expected_root: &BoundDirectoryIdentity,
    _expected: &FreshRootRecoveryCargoInventory,
) -> Result<(), String> {
    Err("fresh-root recovery requires Unix .cargo publication checks".into())
}

#[cfg(unix)]
fn create_fresh_root_recovery_marker(
    root: &Path,
    marker: &WorktreeRootMarker,
    expected_root: &BoundDirectoryIdentity,
) -> Result<crate::worktree_registry::RecoveryArtifactIdentity, String> {
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    if &bound_directory_identity(root)? != expected_root {
        return Err("reserved root identity changed before marker creation".into());
    }
    let path = root.join(crate::broker::WORKTREE_ROOT_MARKER);
    let bytes = fresh_root_recovery_marker_bytes(marker)?;
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let mut file = options
        .open(&path)
        .map_err(|error| format!("cannot create marker without replacement: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    // SAFETY: geteuid has no pointer arguments or caller-side memory preconditions.
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != effective_uid {
        return Err("new recovery marker is not a single-link file owned by this user".into());
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot protect fresh recovery marker: {error}"))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot durably write fresh recovery marker: {error}"))?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    let opened_identity = crate::worktree_registry::RecoveryArtifactIdentity {
        device: opened.dev(),
        inode: opened.ino(),
        owner: opened.uid(),
        mode: opened.mode() & 0o7777,
        links: opened.nlink(),
        bytes: opened.len(),
    };
    let (actual, named_identity) = recovery_path_file_bytes(&path)?;
    if actual != bytes
        || named_identity != opened_identity
        || &bound_directory_identity(root)? != expected_root
    {
        return Err("fresh recovery marker or root changed during publication".into());
    }
    Ok(opened_identity)
}

#[cfg(unix)]
fn sync_fresh_root_recovery_marker_file(
    root: &Path,
    marker: &WorktreeRootMarker,
    expected_root: &BoundDirectoryIdentity,
    expected_file: &crate::worktree_registry::RecoveryArtifactIdentity,
) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    if &bound_directory_identity(root)? != expected_root {
        return Err("reserved root identity changed before marker file sync".into());
    }
    let path = root.join(crate::broker::WORKTREE_ROOT_MARKER);
    let expected_bytes = fresh_root_recovery_marker_bytes(marker)?;
    let (bytes, named_identity) = recovery_path_file_bytes(&path)?;
    if bytes != expected_bytes || &named_identity != expected_file {
        return Err("recovery marker bytes or file identity changed before file sync".into());
    }

    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let file = options
        .open(&path)
        .map_err(|error| format!("cannot open recovery marker without following links: {error}"))?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    let opened_identity = crate::worktree_registry::RecoveryArtifactIdentity {
        device: opened.dev(),
        inode: opened.ino(),
        owner: opened.uid(),
        mode: opened.mode() & 0o7777,
        links: opened.nlink(),
        bytes: opened.len(),
    };
    if !opened.is_file() || &opened_identity != expected_file {
        return Err("recovery marker file identity changed while opening for sync".into());
    }
    file.sync_all()
        .map_err(|error| format!("cannot durably sync recovery marker contents: {error}"))?;

    let (bytes_after, identity_after) = recovery_path_file_bytes(&path)?;
    if bytes_after != expected_bytes
        || &identity_after != expected_file
        || &bound_directory_identity(root)? != expected_root
    {
        return Err("recovery marker or root changed during marker file sync".into());
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_fresh_root_recovery_marker(
    _root: &Path,
    _marker: &WorktreeRootMarker,
    _expected_root: &BoundDirectoryIdentity,
) -> Result<crate::worktree_registry::RecoveryArtifactIdentity, String> {
    Err("fresh-root recovery requires Unix no-follow marker creation".into())
}

#[cfg(not(unix))]
fn sync_fresh_root_recovery_marker_file(
    _root: &Path,
    _marker: &WorktreeRootMarker,
    _expected_root: &BoundDirectoryIdentity,
    _expected_file: &crate::worktree_registry::RecoveryArtifactIdentity,
) -> Result<(), String> {
    Err("fresh-root recovery requires Unix no-follow marker sync".into())
}

#[cfg(not(unix))]
fn fresh_root_recovery_cargo_inventory(
    _directory: &Path,
) -> Result<FreshRootRecoveryCargoInventory, String> {
    Err("fresh-root recovery requires Unix no-follow directory checks".into())
}

#[cfg(all(test, unix))]
mod fresh_root_recovery_inventory_tests {
    use super::*;
    use crate::store::repository_binding::{
        FreshRepositoryRootReservation, RepositoryRootIdentity, RepositoryWorktreeBindingState,
        RepositoryWorktreeRootLayout,
    };
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::Command;

    struct FreshRootFixture {
        _owner: tempfile::TempDir,
        _storage: crate::test_storage::TestStorage,
        broker: Broker,
        root: PathBuf,
        host_operation_database: PathBuf,
        expected_parent: BoundDirectoryIdentity,
        reservation: FreshRepositoryRootReservation,
    }

    fn marker(repository_root: &Path) -> WorktreeRootMarker {
        WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: "test-repository".into(),
            repository_root: repository_root.to_path_buf(),
        }
    }

    fn create_private_directory(path: &Path) {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path).unwrap();
    }

    fn git(repository: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .expect("run fixture git command");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn fresh_root_fixture(root_exists: bool) -> FreshRootFixture {
        let owner = tempfile::tempdir().unwrap();
        let owner_root = owner.path().canonicalize().unwrap();
        let repository = owner_root.join("repository");
        create_private_directory(&repository);
        git(&repository, &["init", "-q", "-b", "main"]);
        git(
            &repository,
            &[
                "-c",
                "user.name=Aethyme Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-qm",
                "fixture",
            ],
        );

        let storage = crate::test_storage::TestStorage::new();
        let host_operation_database = storage.host_state_dir().join("host-operations.db");
        let parent = owner_root.join("managed-roots");
        create_private_directory(&parent);
        let root = parent.join("worktrees");
        if root_exists {
            create_private_directory(&root);
        }
        let options = storage.options().with_worktree_root(root.clone());
        let mut broker = Broker::open_with_options(&repository, options).unwrap();
        let binding = broker.store().repository_worktree_binding().unwrap();
        assert_eq!(binding.state, RepositoryWorktreeBindingState::FreshUnbound);
        let identity = RepositoryRootIdentity {
            repository_root: binding.repository_root.unwrap(),
            git_common_dir: binding.git_common_dir.unwrap(),
            repository_key: binding.repository_key.unwrap(),
            broker_db_path: binding.broker_db_path.unwrap(),
            bootstrap_nonce: binding.bootstrap_nonce.unwrap(),
        };
        let expected_parent = bound_directory_identity(&parent).unwrap();
        let parent_identity_json = serde_json::to_string(&expected_parent).unwrap();
        let layout = RepositoryWorktreeRootLayout::DirectRoot;
        let source = WorktreeRootSource::LibraryOverride;
        let reservation_digest =
            fresh_root_reservation_digest(&identity, &root, layout, source, &parent_identity_json)
                .unwrap();
        let reservation = FreshRepositoryRootReservation {
            identity,
            worktree_root: root.clone(),
            layout,
            source,
            reservation_digest,
            parent_identity_json,
        };
        broker
            .store()
            .reserve_fresh_repository_worktree_root(&reservation)
            .unwrap();
        FreshRootFixture {
            _owner: owner,
            _storage: storage,
            broker,
            root,
            host_operation_database,
            expected_parent,
            reservation,
        }
    }

    fn create_repository_uuid_temporary(common_dir: &Path) -> (PathBuf, String) {
        let uuid = "f47ac10b-58cc-4372-a567-0e02b2c3d479".to_owned();
        let temporary =
            common_dir.join(".aethyme-repository-uuid.0123456789abcdef0123456789abcdef.tmp");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .unwrap();
        file.write_all(format!("AETHYME-REPO-UUID-V1\n{uuid}\n").as_bytes())
            .unwrap();
        file.sync_all().unwrap();
        (temporary, uuid)
    }

    fn replace_root_with_empty_directory(root: &Path) -> PathBuf {
        let moved = root.with_file_name("worktrees-replaced");
        std::fs::rename(root, &moved).unwrap();
        create_private_directory(root);
        moved
    }

    fn assert_reserved_and_unregistered(fixture: &mut FreshRootFixture) {
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
        let plan = fixture
            .broker
            .fresh_worktree_root_recovery_plan()
            .expect("read-only recovery plan after interrupted apply");
        assert_ne!(
            plan.registry.state,
            crate::worktree_registry::FreshRootRegistryState::ExactTargetRow
        );
    }

    fn exact_marker_without_target_row(fixture: &mut FreshRootFixture) -> FreshRootRecoveryPlan {
        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        let error = fixture
            .broker
            .apply_fresh_worktree_root_recovery_with_checkpoint(&plan.digest, |checkpoint, _| {
                if checkpoint
                    == FreshRootRecoveryApplyCheckpoint::MarkerDurablyPublishedBeforeRegistryRecord
                {
                    Err("injected interruption after marker durability".into())
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert!(error.to_string().contains("before registry record"));
        let partial = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            partial.root_inventory.state,
            FreshRootRecoveryPathState::ExactMarker
        );
        assert_eq!(
            partial.registry.state,
            crate::worktree_registry::FreshRootRegistryState::ExistingNoTargetRow
        );
        partial
    }

    #[test]
    fn root_inventory_accepts_only_missing_empty_and_exact_marker_prefixes() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repository");
        create_private_directory(&repository);
        let root = temporary.path().join("reserved-root");
        let expected_marker = marker(&repository);

        assert_eq!(
            fresh_root_recovery_inventory(&root, &expected_marker)
                .unwrap()
                .state,
            FreshRootRecoveryPathState::Missing
        );
        create_private_directory(&root);
        assert_eq!(
            fresh_root_recovery_inventory(&root, &expected_marker)
                .unwrap()
                .state,
            FreshRootRecoveryPathState::Empty
        );
        std::fs::write(
            root.join(crate::broker::WORKTREE_ROOT_MARKER),
            fresh_root_recovery_marker_bytes(&expected_marker).unwrap(),
        )
        .unwrap();
        assert_eq!(
            fresh_root_recovery_inventory(&root, &expected_marker)
                .unwrap()
                .state,
            FreshRootRecoveryPathState::ExactMarker
        );
        std::fs::write(
            root.join(crate::broker::WORKTREE_ROOT_MARKER),
            b"same owner fields, different bytes",
        )
        .unwrap();
        assert!(fresh_root_recovery_inventory(&root, &expected_marker).is_err());
    }

    #[test]
    fn cargo_inventory_accepts_only_empty_or_exact_writer_bytes() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repository");
        create_private_directory(&repository);
        let root = temporary.path().join("reserved-root");
        create_private_directory(&root);
        let expected_marker = marker(&repository);
        std::fs::write(
            root.join(crate::broker::WORKTREE_ROOT_MARKER),
            fresh_root_recovery_marker_bytes(&expected_marker).unwrap(),
        )
        .unwrap();
        let cargo = root.join(".cargo");
        create_private_directory(&cargo);

        assert_eq!(
            fresh_root_recovery_inventory(&root, &expected_marker)
                .unwrap()
                .cargo
                .unwrap()
                .state,
            FreshRootRecoveryCargoState::EmptyDirectory
        );
        let config = cargo.join("config.toml");
        std::fs::write(&config, crate::broker::WORKTREE_CARGO_CONFIG.as_bytes()).unwrap();
        assert_eq!(
            fresh_root_recovery_inventory(&root, &expected_marker)
                .unwrap()
                .cargo
                .unwrap()
                .state,
            FreshRootRecoveryCargoState::ExactConfig
        );
        let temporary_config = cargo.join(".config.123.456.tmp");
        std::fs::rename(&config, &temporary_config).unwrap();
        assert_eq!(
            fresh_root_recovery_inventory(&root, &expected_marker)
                .unwrap()
                .cargo
                .unwrap()
                .state,
            FreshRootRecoveryCargoState::ExactWriterTemporary {
                name: ".config.123.456.tmp".into()
            }
        );
        let noncanonical_temporary = cargo.join(".config.0123.456.tmp");
        std::fs::rename(&temporary_config, &noncanonical_temporary).unwrap();
        assert!(fresh_root_recovery_inventory(&root, &expected_marker).is_err());
        std::fs::rename(&noncanonical_temporary, &temporary_config).unwrap();
        std::fs::write(&config, crate::broker::WORKTREE_CARGO_CONFIG.as_bytes()).unwrap();
        assert!(fresh_root_recovery_inventory(&root, &expected_marker).is_err());
        std::fs::remove_file(&config).unwrap();
        std::fs::write(&temporary_config, b"arbitrary cargo configuration").unwrap();
        assert!(fresh_root_recovery_inventory(&root, &expected_marker).is_err());
    }

    fn write_atomic_cargo_temporary(directory: &Path, bytes: &[u8], mode: u32) -> PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let mut temporary = tempfile::Builder::new()
            .prefix(".config.toml-")
            .tempfile_in(directory)
            .unwrap();
        temporary.write_all(bytes).unwrap();
        temporary.as_file().sync_all().unwrap();
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(mode)).unwrap();
        let (_, path) = temporary.keep().unwrap();
        path
    }

    #[test]
    fn cargo_inventory_recovers_only_exact_atomic_publication_prefixes() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let expected_bytes = crate::broker::WORKTREE_CARGO_CONFIG.as_bytes();
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().join("root");
        create_private_directory(&root);
        let cargo = root.join(".cargo");
        create_private_directory(&cargo);
        let root_identity = bound_directory_identity(&root).unwrap();

        let temporary = write_atomic_cargo_temporary(&cargo, expected_bytes, 0o600);
        let temporary_name = temporary.file_name().unwrap().to_str().unwrap().to_owned();
        let before_link = fresh_root_recovery_cargo_inventory(&cargo).unwrap();
        assert_eq!(
            before_link.state,
            FreshRootRecoveryCargoState::ExactAtomicTemporary {
                name: temporary_name.clone()
            }
        );
        assert_eq!(before_link.file_identity.as_ref().unwrap().links, 1);
        assert_eq!(before_link.file_identity.as_ref().unwrap().mode, 0o600);
        normalize_fresh_root_recovery_cargo(&root, &root_identity, &before_link).unwrap();
        let after_link_recovery = fresh_root_recovery_cargo_inventory(&cargo).unwrap();
        assert_eq!(
            after_link_recovery.state,
            FreshRootRecoveryCargoState::ExactConfig
        );
        assert_eq!(after_link_recovery.file_identity.as_ref().unwrap().links, 1);
        assert_eq!(
            std::fs::read(cargo.join("config.toml")).unwrap(),
            expected_bytes
        );
        assert_eq!(std::fs::read_dir(&cargo).unwrap().count(), 1);

        // Config-only is an idempotent recovery prefix and still syncs the
        // directory before binding is finalized.
        normalize_fresh_root_recovery_cargo(&root, &root_identity, &after_link_recovery).unwrap();

        let paired_root = owner.path().join("paired-root");
        create_private_directory(&paired_root);
        let paired_cargo = paired_root.join(".cargo");
        create_private_directory(&paired_cargo);
        let paired_root_identity = bound_directory_identity(&paired_root).unwrap();
        let paired_temporary = write_atomic_cargo_temporary(&paired_cargo, expected_bytes, 0o600);
        let paired_name = paired_temporary
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        std::fs::hard_link(&paired_temporary, paired_cargo.join("config.toml")).unwrap();
        let after_link = fresh_root_recovery_cargo_inventory(&paired_cargo).unwrap();
        assert_eq!(
            after_link.state,
            FreshRootRecoveryCargoState::ExactAtomicConfigAndTemporary { name: paired_name }
        );
        assert_eq!(after_link.file_identity.as_ref().unwrap().links, 2);
        assert_eq!(
            std::fs::symlink_metadata(paired_cargo.join("config.toml"))
                .unwrap()
                .ino(),
            std::fs::symlink_metadata(&paired_temporary).unwrap().ino()
        );
        normalize_fresh_root_recovery_cargo(&paired_root, &paired_root_identity, &after_link)
            .unwrap();
        let normalized_pair = fresh_root_recovery_cargo_inventory(&paired_cargo).unwrap();
        assert_eq!(
            normalized_pair.state,
            FreshRootRecoveryCargoState::ExactConfig
        );
        assert_eq!(normalized_pair.file_identity.as_ref().unwrap().links, 1);
        assert_eq!(std::fs::read_dir(&paired_cargo).unwrap().count(), 1);

        let legacy_root = owner.path().join("legacy-root");
        create_private_directory(&legacy_root);
        let legacy_cargo = legacy_root.join(".cargo");
        create_private_directory(&legacy_cargo);
        let legacy_root_identity = bound_directory_identity(&legacy_root).unwrap();
        let legacy_temporary = legacy_cargo.join(".config.123.456.tmp");
        std::fs::write(&legacy_temporary, expected_bytes).unwrap();
        std::fs::set_permissions(&legacy_temporary, std::fs::Permissions::from_mode(0o644))
            .unwrap();
        let legacy_prefix = fresh_root_recovery_cargo_inventory(&legacy_cargo).unwrap();
        assert_eq!(
            legacy_prefix.state,
            FreshRootRecoveryCargoState::ExactWriterTemporary {
                name: ".config.123.456.tmp".into()
            }
        );
        normalize_fresh_root_recovery_cargo(&legacy_root, &legacy_root_identity, &legacy_prefix)
            .unwrap();
        assert_eq!(
            fresh_root_recovery_cargo_inventory(&legacy_cargo)
                .unwrap()
                .state,
            FreshRootRecoveryCargoState::ExactConfig
        );
        assert!(!legacy_temporary.exists());

        let legacy_pair_root = owner.path().join("legacy-pair-root");
        create_private_directory(&legacy_pair_root);
        let legacy_pair_cargo = legacy_pair_root.join(".cargo");
        create_private_directory(&legacy_pair_cargo);
        let legacy_pair_root_identity = bound_directory_identity(&legacy_pair_root).unwrap();
        let legacy_pair_temporary = legacy_pair_cargo.join(".config.123.456.tmp");
        std::fs::write(&legacy_pair_temporary, expected_bytes).unwrap();
        std::fs::set_permissions(
            &legacy_pair_temporary,
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        std::fs::hard_link(
            &legacy_pair_temporary,
            legacy_pair_cargo.join("config.toml"),
        )
        .unwrap();
        let legacy_pair_prefix = fresh_root_recovery_cargo_inventory(&legacy_pair_cargo).unwrap();
        assert_eq!(
            legacy_pair_prefix.state,
            FreshRootRecoveryCargoState::ExactWriterConfigAndTemporary {
                name: ".config.123.456.tmp".into()
            }
        );
        normalize_fresh_root_recovery_cargo(
            &legacy_pair_root,
            &legacy_pair_root_identity,
            &legacy_pair_prefix,
        )
        .unwrap();
        assert_eq!(
            fresh_root_recovery_cargo_inventory(&legacy_pair_cargo)
                .unwrap()
                .state,
            FreshRootRecoveryCargoState::ExactConfig
        );

        let drift_root = owner.path().join("drift-root");
        create_private_directory(&drift_root);
        let drift_cargo = drift_root.join(".cargo");
        create_private_directory(&drift_cargo);
        let drift_root_identity = bound_directory_identity(&drift_root).unwrap();
        let drift_temporary = write_atomic_cargo_temporary(&drift_cargo, expected_bytes, 0o600);
        let reviewed = fresh_root_recovery_cargo_inventory(&drift_cargo).unwrap();
        std::fs::write(&drift_temporary, b"partial config").unwrap();
        assert!(
            normalize_fresh_root_recovery_cargo(&drift_root, &drift_root_identity, &reviewed)
                .is_err()
        );
        assert!(!drift_cargo.join("config.toml").exists());

        let invalid_root = owner.path().join("invalid-root");
        create_private_directory(&invalid_root);
        let invalid_cargo = invalid_root.join(".cargo");
        create_private_directory(&invalid_cargo);
        let wrong_mode = write_atomic_cargo_temporary(&invalid_cargo, expected_bytes, 0o640);
        assert!(fresh_root_recovery_cargo_inventory(&invalid_cargo).is_err());
        std::fs::remove_file(wrong_mode).unwrap();
        let wrong_suffix = invalid_cargo.join(".config.toml-not-random");
        std::fs::write(&wrong_suffix, expected_bytes).unwrap();
        std::fs::set_permissions(&wrong_suffix, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(fresh_root_recovery_cargo_inventory(&invalid_cargo).is_err());
        std::fs::remove_file(wrong_suffix).unwrap();

        let mismatch_temporary =
            write_atomic_cargo_temporary(&invalid_cargo, expected_bytes, 0o600);
        let mismatch_config = invalid_cargo.join("config.toml");
        std::fs::write(&mismatch_config, expected_bytes).unwrap();
        std::fs::set_permissions(&mismatch_config, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(fresh_root_recovery_cargo_inventory(&invalid_cargo).is_err());
        std::fs::remove_file(mismatch_config).unwrap();
        std::fs::remove_file(mismatch_temporary).unwrap();

        let partial_temporary =
            write_atomic_cargo_temporary(&invalid_cargo, b"partial config", 0o600);
        assert!(fresh_root_recovery_cargo_inventory(&invalid_cargo).is_err());
        std::fs::remove_file(partial_temporary).unwrap();

        let multiple_first = write_atomic_cargo_temporary(&invalid_cargo, expected_bytes, 0o600);
        let multiple_second = write_atomic_cargo_temporary(&invalid_cargo, expected_bytes, 0o600);
        assert!(fresh_root_recovery_cargo_inventory(&invalid_cargo).is_err());
        std::fs::remove_file(multiple_first).unwrap();
        std::fs::remove_file(multiple_second).unwrap();

        let linked_temporary = write_atomic_cargo_temporary(&invalid_cargo, expected_bytes, 0o600);
        let linked_config = invalid_cargo.join("config.toml");
        std::fs::hard_link(&linked_temporary, &linked_config).unwrap();
        let out_of_directory_link = owner.path().join("cargo-config-extra-link");
        std::fs::hard_link(&linked_temporary, &out_of_directory_link).unwrap();
        assert!(fresh_root_recovery_cargo_inventory(&invalid_cargo).is_err());
        std::fs::remove_file(out_of_directory_link).unwrap();
        std::fs::remove_file(linked_config).unwrap();
        std::fs::remove_file(linked_temporary).unwrap();
    }

    #[test]
    fn recovery_apply_rejects_wrong_digest_without_creating_the_reserved_root() {
        let mut fixture = fresh_root_fixture(false);
        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert!(plan.can_apply);
        assert!(
            fixture
                .broker
                .apply_fresh_worktree_root_recovery(&"0".repeat(64))
                .is_err()
        );
        assert!(!fixture.root.exists());
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
    }

    #[test]
    fn explicit_recovery_open_is_the_only_open_that_accepts_marker_prefixes() {
        let fixture = fresh_root_fixture(false);
        let original_plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        let repository_root = fixture.reservation.identity.repository_root.clone();
        let common_dir = fixture.reservation.identity.git_common_dir.clone();
        let options = fixture
            ._storage
            .options()
            .with_worktree_root(fixture.root.clone());
        drop(fixture.broker);

        let (temporary, uuid) = create_repository_uuid_temporary(&common_dir);
        let marker = common_dir.join("aethyme-repository-uuid");
        let expected_marker = format!("AETHYME-REPO-UUID-V1\n{uuid}\n");
        let graph_fence =
            common_dir.join(aethyme_graph_storage::write_guard::BROKER_MANAGED_GRAPH_MARKER);
        std::fs::remove_file(&graph_fence).unwrap();

        assert!(Broker::open_with_options(&repository_root, options.clone()).is_err());
        assert!(!graph_fence.exists());
        assert!(BrokerStore::open_snapshot_in_repo(&repository_root).is_err());
        assert!(!marker.exists());
        assert_eq!(
            std::fs::read(&temporary).unwrap(),
            expected_marker.as_bytes()
        );

        let mut broker = Broker::open_for_explicit_worktree_root_recovery_with_options(
            &repository_root,
            options,
        )
        .unwrap();
        let recovery_plan = broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(recovery_plan.digest, original_plan.digest);
        assert!(
            broker
                .apply_fresh_worktree_root_recovery(&"0".repeat(64))
                .is_err()
        );
        assert!(!marker.exists());
        assert_eq!(
            std::fs::read(&temporary).unwrap(),
            expected_marker.as_bytes()
        );
        assert!(!fixture.root.exists());

        broker
            .apply_fresh_worktree_root_recovery(&recovery_plan.digest)
            .unwrap();
        assert_eq!(std::fs::read(&marker).unwrap(), expected_marker.as_bytes());
        assert!(!temporary.exists());
        assert!(fixture.root.is_dir());
        assert_eq!(
            broker.store().repository_worktree_binding().unwrap().state,
            RepositoryWorktreeBindingState::Bound
        );
    }

    #[test]
    fn full_registry_refuses_before_creating_a_missing_reserved_root() {
        let mut fixture = fresh_root_fixture(false);
        let admission =
            crate::worktree_registry::admit(&fixture.host_operation_database, &fixture.root)
                .unwrap();
        drop(admission);
        let registry_path =
            crate::worktree_registry::registry_path(&fixture.host_operation_database).unwrap();
        let mut connection = rusqlite::Connection::open(&registry_path).unwrap();
        let transaction = connection.transaction().unwrap();
        for index in 0..crate::worktree_registry::MAX_RECORDS {
            transaction
                .execute(
                    "INSERT INTO roots(key, payload) VALUES(?1, ?2)",
                    rusqlite::params![format!("fixture-{index}"), "{}"],
                )
                .unwrap();
        }
        transaction.commit().unwrap();

        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert!(!plan.can_apply);
        assert_eq!(
            plan.registry.row_count,
            Some(crate::worktree_registry::MAX_RECORDS)
        );
        assert!(
            fixture
                .broker
                .apply_fresh_worktree_root_recovery(&plan.digest)
                .is_err()
        );
        assert!(!fixture.root.exists());
        let count: usize = rusqlite::Connection::open(&registry_path)
            .unwrap()
            .query_row("SELECT count(*) FROM roots", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, crate::worktree_registry::MAX_RECORDS);
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
    }

    #[test]
    fn recovery_marker_is_synced_before_registry_row_and_retry_completes_binding() {
        let mut fixture = fresh_root_fixture(false);
        let first_plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        let events = std::cell::RefCell::new(Vec::new());
        let error = fixture
            .broker
            .apply_fresh_worktree_root_recovery_with_checkpoint_and_sync_and_marker_file(
                &first_plan.digest,
                |checkpoint, _| {
                    if checkpoint
                        == FreshRootRecoveryApplyCheckpoint::MarkerDurablyPublishedBeforeRegistryRecord
                    {
                        events.borrow_mut().push("before-registry-record");
                        Err("injected interruption after marker directory sync".into())
                    } else {
                        Ok(())
                    }
                },
                |path, expected| {
                    if path == fixture.root.as_path() {
                        events.borrow_mut().push("marker-root-sync");
                    }
                    sync_bound_directory(path, expected)
                },
                |root, marker, expected_root, expected_file| {
                    events.borrow_mut().push("marker-file-sync");
                    sync_fresh_root_recovery_marker_file(
                        root,
                        marker,
                        expected_root,
                        expected_file,
                    )
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("before registry record"));
        assert_eq!(
            *events.borrow(),
            vec![
                "marker-file-sync",
                "marker-root-sync",
                "before-registry-record"
            ]
        );
        let partial_plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            partial_plan.root_inventory.state,
            FreshRootRecoveryPathState::ExactMarker
        );
        assert_eq!(
            partial_plan.registry.state,
            crate::worktree_registry::FreshRootRegistryState::ExistingNoTargetRow
        );
        assert_reserved_and_unregistered(&mut fixture);

        fixture
            .broker
            .apply_fresh_worktree_root_recovery(&partial_plan.digest)
            .unwrap();
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::Bound
        );
    }

    #[test]
    fn recovery_syncs_existing_empty_root_parent_before_registry_initialization() {
        let mut fixture = fresh_root_fixture(true);
        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(plan.root_inventory.state, FreshRootRecoveryPathState::Empty);
        let events = std::cell::RefCell::new(Vec::new());
        let error = fixture
            .broker
            .apply_fresh_worktree_root_recovery_with_checkpoint_and_sync(
                &plan.digest,
                |checkpoint, _| {
                    if checkpoint == FreshRootRecoveryApplyCheckpoint::BeforeRegistryInitialization
                    {
                        events.borrow_mut().push("before-registry-initialization");
                    }
                    if checkpoint
                        == FreshRootRecoveryApplyCheckpoint::MarkerDurablyPublishedBeforeRegistryRecord
                    {
                        events.borrow_mut().push("before-registry-record");
                        Err("injected interruption after marker directory sync".into())
                    } else {
                        Ok(())
                    }
                },
                |path, expected| {
                    if path == fixture.root.parent().unwrap() {
                        events.borrow_mut().push("parent-sync");
                    } else if path == fixture.root.as_path() {
                        events.borrow_mut().push("marker-root-sync");
                    }
                    sync_bound_directory(path, expected)
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("before registry record"));
        assert_eq!(
            *events.borrow(),
            vec![
                "parent-sync",
                "before-registry-initialization",
                "marker-root-sync",
                "before-registry-record"
            ]
        );
        let partial_plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            partial_plan.registry.state,
            crate::worktree_registry::FreshRootRegistryState::ExistingNoTargetRow
        );
        assert_reserved_and_unregistered(&mut fixture);
    }

    #[test]
    fn recovery_parent_sync_failure_on_existing_empty_root_leaves_no_target_row() {
        let mut fixture = fresh_root_fixture(true);
        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        let error = fixture
            .broker
            .apply_fresh_worktree_root_recovery_with_checkpoint_and_sync(
                &plan.digest,
                |_, _| Ok(()),
                |path, expected| {
                    if path == fixture.root.parent().unwrap() {
                        Err("injected parent directory sync failure".into())
                    } else {
                        sync_bound_directory(path, expected)
                    }
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("reviewed recovery root parent"));
        let after = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            after.root_inventory.state,
            FreshRootRecoveryPathState::Empty
        );
        assert_eq!(
            after.registry.state,
            crate::worktree_registry::FreshRootRegistryState::MissingDatabase
        );
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
    }

    #[test]
    fn recovery_marker_directory_sync_failure_leaves_no_target_row() {
        let mut fixture = fresh_root_fixture(false);
        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        let error = fixture
            .broker
            .apply_fresh_worktree_root_recovery_with_checkpoint_and_sync(
                &plan.digest,
                |_, _| Ok(()),
                |path, expected| {
                    if path == fixture.root.as_path() {
                        Err("injected marker directory sync failure".into())
                    } else {
                        sync_bound_directory(path, expected)
                    }
                },
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("durably sync recovery marker namespace")
        );
        let partial_plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            partial_plan.root_inventory.state,
            FreshRootRecoveryPathState::ExactMarker
        );
        assert_eq!(
            partial_plan.registry.state,
            crate::worktree_registry::FreshRootRegistryState::ExistingNoTargetRow
        );
        assert_reserved_and_unregistered(&mut fixture);
    }

    #[test]
    fn exact_marker_retry_syncs_marker_file_before_root_directory_and_registry_row() {
        let mut fixture = fresh_root_fixture(false);
        let plan = exact_marker_without_target_row(&mut fixture);
        let events = std::cell::RefCell::new(Vec::new());
        let error = fixture
            .broker
            .apply_fresh_worktree_root_recovery_with_checkpoint_and_sync_and_marker_file(
                &plan.digest,
                |checkpoint, _| {
                    if checkpoint
                        == FreshRootRecoveryApplyCheckpoint::MarkerDurablyPublishedBeforeRegistryRecord
                    {
                        events.borrow_mut().push("before-registry-record");
                        Err("injected interruption after marker directory sync".into())
                    } else {
                        Ok(())
                    }
                },
                |path, expected| {
                    if path == fixture.root.as_path() {
                        events.borrow_mut().push("root-directory-sync");
                    }
                    sync_bound_directory(path, expected)
                },
                |root, marker, expected_root, expected_file| {
                    events.borrow_mut().push("marker-file-sync");
                    sync_fresh_root_recovery_marker_file(
                        root,
                        marker,
                        expected_root,
                        expected_file,
                    )
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("before registry record"));
        assert_eq!(
            *events.borrow(),
            vec![
                "marker-file-sync",
                "root-directory-sync",
                "before-registry-record"
            ]
        );
        let retry_plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            retry_plan.registry.state,
            crate::worktree_registry::FreshRootRegistryState::ExistingNoTargetRow
        );
        fixture
            .broker
            .apply_fresh_worktree_root_recovery(&retry_plan.digest)
            .unwrap();
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::Bound
        );
    }

    #[test]
    fn exact_marker_file_sync_failure_leaves_no_target_row() {
        let mut fixture = fresh_root_fixture(false);
        let plan = exact_marker_without_target_row(&mut fixture);
        let events = std::cell::RefCell::new(Vec::new());
        let error = fixture
            .broker
            .apply_fresh_worktree_root_recovery_with_checkpoint_and_sync_and_marker_file(
                &plan.digest,
                |checkpoint, _| {
                    if checkpoint
                        == FreshRootRecoveryApplyCheckpoint::MarkerDurablyPublishedBeforeRegistryRecord
                    {
                        events.borrow_mut().push("before-registry-record");
                        Err("checkpoint ran despite marker file sync failure".into())
                    } else {
                        Ok(())
                    }
                },
                |path, expected| {
                    if path == fixture.root.as_path() {
                        events.borrow_mut().push("root-directory-sync");
                    }
                    sync_bound_directory(path, expected)
                },
                |_, _, _, _| {
                    events.borrow_mut().push("marker-file-sync");
                    Err("injected marker file sync failure".into())
                },
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("durably sync recovery marker file")
        );
        assert_eq!(*events.borrow(), vec!["marker-file-sync"]);
        let after = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            after.root_inventory.state,
            FreshRootRecoveryPathState::ExactMarker
        );
        assert_eq!(
            after.registry.state,
            crate::worktree_registry::FreshRootRegistryState::ExistingNoTargetRow
        );
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
    }

    #[test]
    fn exact_marker_replacement_after_plan_is_refused_even_with_same_bytes() {
        let mut fixture = fresh_root_fixture(false);
        let plan = exact_marker_without_target_row(&mut fixture);
        let marker_path = fixture.root.join(WORKTREE_ROOT_MARKER);
        let previous_marker = fixture.root.with_file_name("previous-marker");
        let marker_bytes = std::fs::read(&marker_path).unwrap();
        std::fs::rename(&marker_path, &previous_marker).unwrap();
        std::fs::write(&marker_path, marker_bytes).unwrap();

        let after_replacement = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            after_replacement.root_inventory.state,
            FreshRootRecoveryPathState::ExactMarker
        );
        assert_ne!(
            after_replacement.root_inventory.marker_identity,
            plan.root_inventory.marker_identity
        );
        assert!(
            fixture
                .broker
                .apply_fresh_worktree_root_recovery(&plan.digest)
                .is_err()
        );
        assert_eq!(
            after_replacement.registry.state,
            crate::worktree_registry::FreshRootRegistryState::ExistingNoTargetRow
        );
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
    }

    #[test]
    fn durable_registry_row_before_binding_is_an_exact_retryable_prefix() {
        let mut fixture = fresh_root_fixture(false);
        let first_plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        let error = fixture
            .broker
            .apply_fresh_worktree_root_recovery_with_checkpoint(
                &first_plan.digest,
                |checkpoint, _| {
                    if checkpoint
                        == FreshRootRecoveryApplyCheckpoint::RegistryRowDurablyRecordedBeforeBinding
                    {
                        Err("injected interruption after registry durability".into())
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("before binding"));
        let partial_plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            partial_plan.registry.state,
            crate::worktree_registry::FreshRootRegistryState::ExactTargetRow
        );
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
        fixture
            .broker
            .apply_fresh_worktree_root_recovery(&partial_plan.digest)
            .unwrap();
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::Bound
        );
    }

    #[test]
    fn recovery_apply_refuses_replacement_of_reviewed_empty_root_after_registry_initialization() {
        let mut fixture = fresh_root_fixture(true);
        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        let error = fixture
            .broker
            .apply_fresh_worktree_root_recovery_with_checkpoint(&plan.digest, |checkpoint, path| {
                if checkpoint
                    == FreshRootRecoveryApplyCheckpoint::AfterRegistryInitializationBeforeMarker
                {
                    replace_root_with_empty_directory(path);
                }
                Ok(())
            })
            .unwrap_err();
        assert!(error.to_string().contains("identity"));
        assert!(!fixture.root.join(WORKTREE_ROOT_MARKER).exists());
        let after = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            after.registry.state,
            crate::worktree_registry::FreshRootRegistryState::ExistingNoTargetRow
        );
        assert_eq!(
            after.root_inventory.state,
            FreshRootRecoveryPathState::Empty
        );
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
    }

    #[test]
    fn recovery_apply_refuses_replacement_of_created_missing_root_before_admission() {
        let mut fixture = fresh_root_fixture(false);
        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        let error = fixture
            .broker
            .apply_fresh_worktree_root_recovery_with_checkpoint(&plan.digest, |checkpoint, path| {
                if checkpoint == FreshRootRecoveryApplyCheckpoint::BeforeRegistryInitialization {
                    replace_root_with_empty_directory(path);
                }
                Ok(())
            })
            .unwrap_err();
        assert!(error.to_string().contains("identity"));
        assert!(!fixture.root.join(WORKTREE_ROOT_MARKER).exists());
        let after = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            after.registry.state,
            crate::worktree_registry::FreshRootRegistryState::MissingDatabase
        );
        assert_eq!(
            after.root_inventory.state,
            FreshRootRecoveryPathState::Empty
        );
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
    }

    #[test]
    fn recovery_apply_refuses_replacement_after_marker_sync_before_registry_row() {
        let mut fixture = fresh_root_fixture(true);
        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        let mut moved_root = None;
        let error = fixture
            .broker
            .apply_fresh_worktree_root_recovery_with_checkpoint(&plan.digest, |checkpoint, path| {
                if checkpoint
                    == FreshRootRecoveryApplyCheckpoint::MarkerDurablyPublishedBeforeRegistryRecord
                {
                    moved_root = Some(replace_root_with_empty_directory(path));
                }
                Ok(())
            })
            .unwrap_err();
        assert!(error.to_string().contains("identity"));
        let moved_root = moved_root.unwrap();
        assert!(moved_root.join(WORKTREE_ROOT_MARKER).is_file());
        assert!(!fixture.root.join(WORKTREE_ROOT_MARKER).exists());
        assert_reserved_and_unregistered(&mut fixture);
    }

    #[test]
    fn recovery_apply_preserves_durable_exact_row_when_root_changes_before_binding() {
        let mut fixture = fresh_root_fixture(false);
        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        let mut moved_root = None;
        let mut recorded_payload = None;
        let error = fixture
            .broker
            .apply_fresh_worktree_root_recovery_with_checkpoint(&plan.digest, |checkpoint, path| {
                if checkpoint
                    == FreshRootRecoveryApplyCheckpoint::RegistryRowDurablyRecordedBeforeBinding
                {
                    let registry_path =
                        crate::worktree_registry::registry_path(&fixture.host_operation_database)
                            .unwrap();
                    recorded_payload = Some(
                        rusqlite::Connection::open(registry_path)
                            .unwrap()
                            .query_row("SELECT payload FROM roots", [], |row| {
                                row.get::<_, String>(0)
                            })
                            .unwrap(),
                    );
                    moved_root = Some(replace_root_with_empty_directory(path));
                }
                Ok(())
            })
            .unwrap_err();
        assert!(error.to_string().contains("identity"));
        assert!(moved_root.unwrap().join(WORKTREE_ROOT_MARKER).is_file());
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
        let registry_path =
            crate::worktree_registry::registry_path(&fixture.host_operation_database).unwrap();
        let persisted_payload: String = rusqlite::Connection::open(registry_path)
            .unwrap()
            .query_row("SELECT payload FROM roots", [], |row| row.get(0))
            .unwrap();
        assert_eq!(Some(persisted_payload), recorded_payload);
    }

    #[test]
    fn recovery_apply_refuses_parent_replacement_before_binding_and_preserves_row() {
        let mut fixture = fresh_root_fixture(true);
        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        let mut recorded_payload = None;
        let mut root_identity_before_replacement = None;
        let error = fixture
            .broker
            .apply_fresh_worktree_root_recovery_with_checkpoint(&plan.digest, |checkpoint, path| {
                if checkpoint
                    == FreshRootRecoveryApplyCheckpoint::RegistryRowDurablyRecordedBeforeBinding
                {
                    root_identity_before_replacement =
                        Some(bound_directory_identity(path).unwrap());
                    let registry_path =
                        crate::worktree_registry::registry_path(&fixture.host_operation_database)
                            .unwrap();
                    recorded_payload = Some(
                        rusqlite::Connection::open(registry_path)
                            .unwrap()
                            .query_row("SELECT payload FROM roots", [], |row| {
                                row.get::<_, String>(0)
                            })
                            .unwrap(),
                    );

                    let parent = path.parent().unwrap().to_path_buf();
                    let previous_parent = parent.with_file_name("managed-roots-replaced");
                    std::fs::rename(&parent, &previous_parent).unwrap();
                    create_private_directory(&parent);
                    std::fs::rename(previous_parent.join("worktrees"), path).unwrap();
                    assert_eq!(
                        bound_directory_identity(path).unwrap(),
                        root_identity_before_replacement.clone().unwrap()
                    );
                }
                Ok(())
            })
            .unwrap_err();
        assert!(error.to_string().contains("parent changed before binding"));
        assert_eq!(
            Some(bound_directory_identity(&fixture.root).unwrap()),
            root_identity_before_replacement
        );
        assert!(fixture.root.join(WORKTREE_ROOT_MARKER).is_file());
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
        let registry_path =
            crate::worktree_registry::registry_path(&fixture.host_operation_database).unwrap();
        let persisted_payload: String = rusqlite::Connection::open(registry_path)
            .unwrap()
            .query_row("SELECT payload FROM roots", [], |row| row.get(0))
            .unwrap();
        assert_eq!(Some(persisted_payload), recorded_payload);
    }

    #[test]
    fn normal_fresh_writer_syncs_marker_before_record_and_pins_created_inode() {
        let mut fixture = fresh_root_fixture(false);
        let events = std::cell::RefCell::new(Vec::new());
        let error = fixture
            .broker
            .finish_fresh_worktree_root_with_checkpoint_and_sync(
                &fixture.reservation.clone(),
                &fixture.expected_parent,
                |checkpoint, _| {
                    if checkpoint
                        == FreshRootPublicationCheckpoint::MarkerDurablyPublishedBeforeRegistryRecord
                    {
                        events.borrow_mut().push("before-registry-record");
                        Err("injected interruption after marker directory sync".into())
                    } else {
                        Ok(())
                    }
                },
                |path, expected| {
                    if path == fixture.root.as_path() {
                        events.borrow_mut().push("marker-root-sync");
                    }
                    sync_bound_directory(path, expected)
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("before registry record"));
        assert_eq!(
            *events.borrow(),
            vec!["marker-root-sync", "before-registry-record"]
        );
        assert!(fixture.root.join(WORKTREE_ROOT_MARKER).is_file());
        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            plan.registry.state,
            crate::worktree_registry::FreshRootRegistryState::ExistingNoTargetRow
        );
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
    }

    #[test]
    fn normal_fresh_writer_marker_directory_sync_failure_leaves_no_target_row() {
        let mut fixture = fresh_root_fixture(false);
        let error = fixture
            .broker
            .finish_fresh_worktree_root_with_checkpoint_and_sync(
                &fixture.reservation.clone(),
                &fixture.expected_parent,
                |_, _| Ok(()),
                |path, expected| {
                    if path == fixture.root.as_path() {
                        Err("injected marker directory sync failure".into())
                    } else {
                        sync_bound_directory(path, expected)
                    }
                },
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("durably sync worktree-root marker namespace")
        );
        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            plan.root_inventory.state,
            FreshRootRecoveryPathState::ExactMarker
        );
        assert_eq!(
            plan.registry.state,
            crate::worktree_registry::FreshRootRegistryState::ExistingNoTargetRow
        );
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
    }

    #[test]
    fn normal_fresh_writer_refuses_replacement_after_marker_sync_before_record() {
        let mut fixture = fresh_root_fixture(false);
        let mut moved_root = None;
        let error = fixture
            .broker
            .finish_fresh_worktree_root_with_checkpoint(
                &fixture.reservation.clone(),
                &fixture.expected_parent,
                |checkpoint, path| {
                    if checkpoint
                        == FreshRootPublicationCheckpoint::MarkerDurablyPublishedBeforeRegistryRecord
                    {
                        moved_root = Some(replace_root_with_empty_directory(path));
                    }
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("identity"));
        assert!(moved_root.unwrap().join(WORKTREE_ROOT_MARKER).is_file());
        assert!(!fixture.root.join(WORKTREE_ROOT_MARKER).exists());
        let plan = fixture.broker.fresh_worktree_root_recovery_plan().unwrap();
        assert_eq!(
            plan.registry.state,
            crate::worktree_registry::FreshRootRegistryState::ExistingNoTargetRow
        );
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
    }

    #[test]
    fn normal_fresh_writer_refuses_replacement_after_row_before_build_defaults() {
        let mut fixture = fresh_root_fixture(false);
        let mut moved_root = None;
        let mut recorded_payload = None;
        let error = fixture
            .broker
            .finish_fresh_worktree_root_with_checkpoint(
                &fixture.reservation.clone(),
                &fixture.expected_parent,
                |checkpoint, path| {
                    if checkpoint == FreshRootPublicationCheckpoint::RegistryRowDurablyRecorded {
                        let registry_path = crate::worktree_registry::registry_path(
                            &fixture.host_operation_database,
                        )
                        .unwrap();
                        recorded_payload = Some(
                            rusqlite::Connection::open(registry_path)
                                .unwrap()
                                .query_row("SELECT payload FROM roots", [], |row| {
                                    row.get::<_, String>(0)
                                })
                                .unwrap(),
                        );
                        moved_root = Some(replace_root_with_empty_directory(path));
                    }
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("identity"));
        let moved_root = moved_root.unwrap();
        assert!(moved_root.join(WORKTREE_ROOT_MARKER).is_file());
        assert_eq!(std::fs::read_dir(&fixture.root).unwrap().count(), 0);
        let registry_path =
            crate::worktree_registry::registry_path(&fixture.host_operation_database).unwrap();
        let persisted_payload: String = rusqlite::Connection::open(registry_path)
            .unwrap()
            .query_row("SELECT payload FROM roots", [], |row| row.get(0))
            .unwrap();
        assert_eq!(Some(persisted_payload), recorded_payload);
        assert_eq!(
            fixture
                .broker
                .store()
                .repository_worktree_binding()
                .unwrap()
                .state,
            RepositoryWorktreeBindingState::FreshReserved
        );
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct LegacyWorktreeRootBindingPlan {
    pub(crate) schema_version: u32,
    pub(crate) repository_root: PathBuf,
    pub(crate) git_common_dir: PathBuf,
    pub(crate) repository_key: String,
    pub(crate) broker_db_path: PathBuf,
    pub(crate) worktree_root: PathBuf,
    pub(crate) layout: String,
    pub(crate) source: WorktreeRootSource,
    pub(crate) parent_identity: BoundDirectoryIdentity,
    pub(crate) worktree_identity: BoundDirectoryIdentity,
    pub(crate) digest: String,
}

fn fresh_root_identity(
    binding: &crate::store::repository_binding::RepositoryWorktreeBinding,
    main_root: &Path,
    repo: &GitRepo,
    database_path: &Path,
) -> Result<crate::store::repository_binding::RepositoryRootIdentity, BrokerOpError> {
    use crate::store::repository_binding::{
        RepositoryRootIdentity, RepositoryWorktreeBindingOrigin, RepositoryWorktreeBindingState,
    };

    let refusal = |reason: String| BrokerOpError::WorktreeRootUnavailable {
        path: main_root.to_path_buf(),
        reason,
    };
    if binding.state != RepositoryWorktreeBindingState::FreshUnbound
        || binding.origin != Some(RepositoryWorktreeBindingOrigin::Fresh)
    {
        return Err(refusal(
            "only a fresh-unbound database may reserve its first root".into(),
        ));
    }
    let repository_root = binding
        .repository_root
        .clone()
        .ok_or_else(|| refusal("fresh binding has no repository root".into()))?;
    let git_common_dir = binding
        .git_common_dir
        .clone()
        .ok_or_else(|| refusal("fresh binding has no Git common directory".into()))?;
    let repository_key = binding
        .repository_key
        .clone()
        .ok_or_else(|| refusal("fresh binding has no repository key".into()))?;
    let broker_db_path = binding
        .broker_db_path
        .clone()
        .ok_or_else(|| refusal("fresh binding has no broker database path".into()))?;
    let bootstrap_nonce = binding
        .bootstrap_nonce
        .clone()
        .ok_or_else(|| refusal("fresh binding has no bootstrap nonce".into()))?;
    let current_repository_root = main_root
        .canonicalize()
        .map_err(|error| refusal(format!("cannot verify repository root: {error}")))?;
    let current_git_common_dir = repo
        .git_common_dir()?
        .canonicalize()
        .map_err(|error| refusal(format!("cannot verify Git common directory: {error}")))?;
    let current_repository_key =
        crate::host_state::repository_key(&current_repository_root, Some(&current_git_common_dir));
    let current_database_path = database_path
        .canonicalize()
        .map_err(|error| refusal(format!("cannot verify broker database path: {error}")))?;
    if repository_root != current_repository_root
        || git_common_dir != current_git_common_dir
        || repository_key != current_repository_key
        || broker_db_path != current_database_path
    {
        return Err(refusal(
            "fresh database identity differs from this repository or database path".into(),
        ));
    }
    Ok(RepositoryRootIdentity {
        repository_root,
        git_common_dir,
        repository_key,
        broker_db_path,
        bootstrap_nonce,
    })
}

fn fresh_root_recovery_identity(
    binding: &crate::store::repository_binding::RepositoryWorktreeBinding,
    main_root: &Path,
    repo: &GitRepo,
    database_path: &Path,
) -> Result<
    (
        crate::store::repository_binding::RepositoryRootIdentity,
        PathBuf,
        crate::store::repository_binding::RepositoryWorktreeRootLayout,
        WorktreeRootSource,
        BoundDirectoryIdentity,
    ),
    BrokerOpError,
> {
    use crate::store::repository_binding::{
        RepositoryRootIdentity, RepositoryWorktreeBindingOrigin, RepositoryWorktreeBindingState,
    };

    let refuse = |reason: String| BrokerOpError::WorktreeRootUnavailable {
        path: binding
            .worktree_root
            .clone()
            .unwrap_or_else(|| main_root.to_path_buf()),
        reason,
    };
    if !matches!(
        binding.state,
        RepositoryWorktreeBindingState::FreshReserved
            | RepositoryWorktreeBindingState::RecoveryRequired
    ) || binding.origin != Some(RepositoryWorktreeBindingOrigin::Fresh)
    {
        return Err(refuse(
            "fresh-root recovery requires a fresh_reserved or recovery_required binding".into(),
        ));
    }
    let repository_root = binding
        .repository_root
        .clone()
        .ok_or_else(|| refuse("fresh reservation has no repository root".into()))?;
    let git_common_dir = binding
        .git_common_dir
        .clone()
        .ok_or_else(|| refuse("fresh reservation has no Git common directory".into()))?;
    let repository_key = binding
        .repository_key
        .clone()
        .ok_or_else(|| refuse("fresh reservation has no repository key".into()))?;
    let broker_db_path = binding
        .broker_db_path
        .clone()
        .ok_or_else(|| refuse("fresh reservation has no broker database path".into()))?;
    let bootstrap_nonce = binding
        .bootstrap_nonce
        .clone()
        .ok_or_else(|| refuse("fresh reservation has no bootstrap nonce".into()))?;
    let root = binding
        .worktree_root
        .clone()
        .ok_or_else(|| refuse("fresh reservation has no exact worktree root".into()))?;
    let layout = binding
        .root_layout
        .ok_or_else(|| refuse("fresh reservation has no root layout".into()))?;
    let source = binding
        .root_source
        .ok_or_else(|| refuse("fresh reservation has no root source".into()))?;
    let reservation_digest = binding
        .reservation_digest
        .as_deref()
        .ok_or_else(|| refuse("fresh reservation has no reservation digest".into()))?;
    let parent_json = binding
        .parent_identity_json
        .as_deref()
        .ok_or_else(|| refuse("fresh reservation has no parent identity".into()))?;
    if binding.worktree_identity_json.is_some() {
        return Err(refuse(
            "incomplete fresh reservation unexpectedly contains a bound root identity".into(),
        ));
    }
    let expected_parent: BoundDirectoryIdentity = serde_json::from_str(parent_json)
        .map_err(|error| refuse(format!("reserved parent identity is invalid: {error}")))?;
    let parent = root
        .parent()
        .ok_or_else(|| refuse("reserved root has no parent".into()))?;
    if !root.is_absolute() || expected_parent.canonical_path != parent {
        return Err(refuse(
            "reserved root or parent path differs from the persisted exact path".into(),
        ));
    }
    let current_repository_root = main_root
        .canonicalize()
        .map_err(|error| refuse(format!("cannot verify repository root: {error}")))?;
    let current_git_common_dir = repo
        .git_common_dir()?
        .canonicalize()
        .map_err(|error| refuse(format!("cannot verify Git common directory: {error}")))?;
    let current_repository_key =
        crate::host_state::repository_key(&current_repository_root, Some(&current_git_common_dir));
    let current_database_path = database_path
        .canonicalize()
        .map_err(|error| refuse(format!("cannot verify broker database path: {error}")))?;
    if repository_root != current_repository_root
        || git_common_dir != current_git_common_dir
        || repository_key != current_repository_key
        || broker_db_path != current_database_path
    {
        return Err(refuse(
            "fresh reservation identity differs from this repository or broker database".into(),
        ));
    }
    let identity = RepositoryRootIdentity {
        repository_root,
        git_common_dir,
        repository_key,
        broker_db_path,
        bootstrap_nonce,
    };
    let recomputed_reservation =
        fresh_root_reservation_digest(&identity, &root, layout, source, parent_json)?;
    if recomputed_reservation != reservation_digest {
        return Err(refuse(
            "fresh reservation digest does not match its stored root, layout, source, or parent identity".into(),
        ));
    }
    if canonical_prefix(&root)
        .map_err(|reason| refuse(format!("cannot resolve reserved root: {reason}")))?
        != root
    {
        return Err(refuse(
            "reserved root path is not its canonical unresolved name".into(),
        ));
    }
    let actual_parent = bound_directory_identity(parent)
        .map_err(|reason| refuse(format!("cannot verify reserved root parent: {reason}")))?;
    if actual_parent != expected_parent {
        return Err(refuse("reserved root parent identity changed".into()));
    }
    Ok((identity, root, layout, source, expected_parent))
}

fn fresh_root_reservation_digest(
    identity: &crate::store::repository_binding::RepositoryRootIdentity,
    root: &Path,
    layout: crate::store::repository_binding::RepositoryWorktreeRootLayout,
    source: WorktreeRootSource,
    parent_identity_json: &str,
) -> Result<String, BrokerOpError> {
    #[derive(serde::Serialize)]
    struct FreshRootReservationDigestInput<'a> {
        repository_root: &'a Path,
        git_common_dir: &'a Path,
        repository_key: &'a str,
        broker_db_path: &'a Path,
        bootstrap_nonce: &'a str,
        worktree_root: &'a Path,
        layout: String,
        source: WorktreeRootSource,
        parent_identity: &'a str,
    }

    let payload = FreshRootReservationDigestInput {
        repository_root: &identity.repository_root,
        git_common_dir: &identity.git_common_dir,
        repository_key: &identity.repository_key,
        broker_db_path: &identity.broker_db_path,
        bootstrap_nonce: &identity.bootstrap_nonce,
        worktree_root: root,
        layout: format!("{layout:?}"),
        source,
        parent_identity: parent_identity_json,
    };
    let bytes = canonical_json_value_bytes(payload).map_err(|error| {
        BrokerOpError::WorktreeRootUnavailable {
            path: root.to_path_buf(),
            reason: format!("cannot encode root reservation identity: {error}"),
        }
    })?;
    Ok(hex_digest(&bytes))
}

/// Serialize a typed digest payload through a JSON value so object keys keep
/// serde_json's canonical map ordering. Unlike `json!`, this reports invalid
/// path encodings instead of panicking while converting a `PathBuf`.
fn canonical_json_value_bytes<T: serde::Serialize>(
    payload: T,
) -> Result<Vec<u8>, serde_json::Error> {
    let value = serde_json::to_value(payload)?;
    serde_json::to_vec(&value)
}

struct LegacyWorktreeRootBindingDigestInput<'a> {
    repository_root: &'a Path,
    git_common_dir: &'a Path,
    repository_key: &'a str,
    broker_db_path: &'a Path,
    worktree_root: &'a Path,
    layout: &'a str,
    source: WorktreeRootSource,
    parent_identity: &'a str,
    worktree_identity: &'a str,
}

fn legacy_worktree_root_binding_digest(
    input: LegacyWorktreeRootBindingDigestInput<'_>,
) -> Result<String, serde_json::Error> {
    #[derive(serde::Serialize)]
    struct LegacyBindingDigestInput<'a> {
        schema_version: u32,
        repository_root: &'a Path,
        git_common_dir: &'a Path,
        repository_key: &'a str,
        broker_db_path: &'a Path,
        worktree_root: &'a Path,
        layout: &'a str,
        source: WorktreeRootSource,
        parent_identity: &'a str,
        worktree_identity: &'a str,
    }

    let payload = LegacyBindingDigestInput {
        schema_version: 1,
        repository_root: input.repository_root,
        git_common_dir: input.git_common_dir,
        repository_key: input.repository_key,
        broker_db_path: input.broker_db_path,
        worktree_root: input.worktree_root,
        layout: input.layout,
        source: input.source,
        parent_identity: input.parent_identity,
        worktree_identity: input.worktree_identity,
    };
    canonical_json_value_bytes(payload).map(|bytes| hex_digest(&bytes))
}

fn bound_directory_identity(path: &Path) -> Result<BoundDirectoryIdentity, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("path is not a real directory".into());
    }
    let canonical_path = path.canonicalize().map_err(|error| error.to_string())?;
    if canonical_path != path {
        return Err("path is not its canonical directory name".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(BoundDirectoryIdentity {
            canonical_path,
            device: metadata.dev(),
            inode: metadata.ino(),
            uid: metadata.uid(),
        })
    }
    #[cfg(not(unix))]
    {
        Err(format!(
            "this platform cannot verify the persisted directory identity for {}",
            canonical_path.display()
        ))
    }
}

fn require_bound_directory_identity(
    path: &Path,
    expected: &BoundDirectoryIdentity,
) -> Result<(), String> {
    if &bound_directory_identity(path)? == expected {
        Ok(())
    } else {
        Err("directory identity changed at the reserved path".into())
    }
}

/// The bound worktree root is private state, not just a stable directory name.
/// Its owner may read/write it, but group/other write access would allow another
/// local user to alter retained worktrees after identity validation.
#[cfg(unix)]
fn verify_private_worktree_root_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("path is not a real directory".into());
    }
    let mode = metadata.permissions().mode() & 0o7777;
    if mode & 0o022 != 0 {
        return Err(format!(
            "worktree root is group- or world-writable (mode {mode:04o})"
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_private_worktree_root_permissions(_path: &Path) -> Result<(), String> {
    Err("this platform cannot verify private worktree-root permissions".into())
}

#[cfg(not(unix))]
fn sync_bound_directory(_path: &Path, _expected: &BoundDirectoryIdentity) -> Result<(), String> {
    Err("durable fresh-root publication requires Unix directory syncing".into())
}

#[cfg(unix)]
fn sync_bound_directory(path: &Path, expected: &BoundDirectoryIdentity) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    if &bound_directory_identity(path)? != expected {
        return Err("directory identity changed before syncing its entries".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let directory = options.open(path).map_err(|error| error.to_string())?;
    let metadata = directory.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_dir()
        || metadata.dev() != expected.device
        || metadata.ino() != expected.inode
        || metadata.uid() != expected.uid
    {
        return Err("opened directory identity differs from the reserved identity".into());
    }
    directory.sync_all().map_err(|error| error.to_string())?;
    if &bound_directory_identity(path)? != expected {
        return Err("directory identity changed while syncing its entries".into());
    }
    Ok(())
}

impl Broker {
    pub fn open(path_inside_repo: &Path) -> Result<Self, BrokerOpError> {
        Self::open_with_options(path_inside_repo, BrokerOpenOptions::default())
    }

    /// Open for explicit worktree-root recovery without running startup
    /// maintenance. Recovery plans and confirmed applies may inspect a valid
    /// but incomplete repository UUID marker publication; ordinary and
    /// read-only opens remain strict and non-repairing.
    pub(crate) fn open_for_explicit_worktree_root_recovery(
        path_inside_repo: &Path,
    ) -> Result<Self, BrokerOpError> {
        Self::open_for_explicit_worktree_root_recovery_with_options(
            path_inside_repo,
            BrokerOpenOptions::default(),
        )
    }

    pub(crate) fn open_for_explicit_worktree_root_recovery_with_options(
        path_inside_repo: &Path,
        options: BrokerOpenOptions,
    ) -> Result<Self, BrokerOpError> {
        let mut storage_context = BrokerStorageContext::capture(options);
        storage_context.ensure_valid()?;
        let here = GitRepo::discover(path_inside_repo)?;
        let main_root = here.main_root()?;
        let repo = GitRepo::discover(&main_root)?;
        storage_context.bind_repository(&repo)?;
        let store = BrokerStore::open_for_explicit_root_recovery(&main_root)?;
        Ok(Self {
            repo,
            store,
            main_root,
            graph_impact_provider: Box::new(GraphStoreImpactProvider),
            storage_context,
            host_operation_db_path: None,
            worktree_root_override: None,
            agent_supervisor_executable: None,
            collaboration_state_override: None,
            landing_deadline: std::cell::Cell::new(None),
            main_common_dir: std::cell::OnceCell::new(),
            artifact_sweep_namespace: std::cell::OnceCell::new(),
            #[cfg(test)]
            submission_plan_test_observer: None,
            #[cfg(test)]
            worktree_lifecycle_test_observer: None,
        })
    }

    /// Open a broker with storage locations captured before startup maintenance.
    ///
    /// The returned handle and its recovery, worktree, preparation-cache, and
    /// gate-cache operations use the same host paths.
    pub fn open_with_options(
        path_inside_repo: &Path,
        options: BrokerOpenOptions,
    ) -> Result<Self, BrokerOpError> {
        Self::open_with_graph_impact_provider_and_options(
            path_inside_repo,
            GraphStoreImpactProvider,
            options,
        )
    }

    /// Open a broker for compatibility diagnostics without changing the
    /// repository database, contracts, prepared operations, events, or refs.
    /// Readable older storage is copied and migrated only in a temporary
    /// snapshot owned by this broker instance.
    pub fn open_snapshot(path_inside_repo: &Path) -> Result<Self, BrokerOpError> {
        let mut storage_context = BrokerStorageContext::capture(BrokerOpenOptions::default());
        storage_context.ensure_valid()?;
        let here = GitRepo::discover(path_inside_repo)?;
        let main_root = here.main_root()?;
        let repo = GitRepo::discover(&main_root)?;
        storage_context.bind_repository(&repo)?;
        let store = BrokerStore::open_snapshot_in_repo(&main_root)?;
        Ok(Self {
            repo,
            store,
            main_root,
            graph_impact_provider: Box::new(GraphStoreImpactProvider),
            storage_context,
            host_operation_db_path: None,
            worktree_root_override: None,
            agent_supervisor_executable: None,
            collaboration_state_override: None,
            landing_deadline: std::cell::Cell::new(None),
            main_common_dir: std::cell::OnceCell::new(),
            artifact_sweep_namespace: std::cell::OnceCell::new(),
            #[cfg(test)]
            submission_plan_test_observer: None,
            #[cfg(test)]
            worktree_lifecycle_test_observer: None,
        })
    }

    /// Observe current coordination state without startup recovery, GC, or
    /// initialization. Keep the same strict durable root identity checks.
    pub(crate) fn open_status_snapshot(path_inside_repo: &Path) -> Result<Self, BrokerOpError> {
        let broker = Self::open_snapshot(path_inside_repo)?;
        broker.validate_bound_repository_worktree_root()?;
        Ok(broker)
    }

    /// Open only far enough to migrate compatible broker storage and backfill
    /// the immutable contract of live pre-v9 sessions. The CLI uses this for
    /// an explicitly requested session continuation; unlike normal open it
    /// does not reconcile prepared shared operations before compatibility has
    /// authorized the command itself.
    pub fn open_for_compatibility_backfill(path_inside_repo: &Path) -> Result<Self, BrokerOpError> {
        Self::open_for_compatibility_backfill_with_options(
            path_inside_repo,
            BrokerOpenOptions::default(),
        )
    }

    pub(crate) fn open_for_compatibility_backfill_with_options(
        path_inside_repo: &Path,
        options: BrokerOpenOptions,
    ) -> Result<Self, BrokerOpError> {
        let mut storage_context = BrokerStorageContext::capture(options);
        storage_context.ensure_valid()?;
        let here = GitRepo::discover(path_inside_repo)?;
        let main_root = here.main_root()?;
        let repo = GitRepo::discover(&main_root)?;
        storage_context.bind_repository(&repo)?;
        let store = BrokerStore::open_in_repo(&main_root)?;
        let mut broker = Self {
            repo,
            store,
            main_root,
            graph_impact_provider: Box::new(GraphStoreImpactProvider),
            storage_context,
            host_operation_db_path: None,
            worktree_root_override: None,
            agent_supervisor_executable: None,
            collaboration_state_override: None,
            landing_deadline: std::cell::Cell::new(None),
            main_common_dir: std::cell::OnceCell::new(),
            artifact_sweep_namespace: std::cell::OnceCell::new(),
            #[cfg(test)]
            submission_plan_test_observer: None,
            #[cfg(test)]
            worktree_lifecycle_test_observer: None,
        };
        // Opening a schema-54 database with a legacy_unbound row does not run
        // root-dependent startup maintenance. It must not initialize a shared
        // preparation-cache root or recover from a root selected from today's
        // environment. Those writes resume only after a reviewed binding exists.
        if broker.validate_bound_repository_worktree_root()?.is_none() {
            return Ok(broker);
        }
        broker
            .storage_context
            .initialize_preparation_cache_root(&broker.repo)?;
        broker.backfill_live_repository_contracts()?;
        Ok(broker)
    }

    /// Open a broker with an alternate read-only graph-impact provider.
    /// Provider outcomes remain confined to [`Self::semantic_gate_advice`].
    pub fn open_with_graph_impact_provider(
        path_inside_repo: &Path,
        graph_impact_provider: impl GraphImpactProvider + 'static,
    ) -> Result<Self, BrokerOpError> {
        Self::open_with_graph_impact_provider_and_options(
            path_inside_repo,
            graph_impact_provider,
            BrokerOpenOptions::default(),
        )
    }

    /// Open a broker with an alternate read-only graph-impact provider and
    /// explicit host storage locations captured before startup maintenance.
    pub fn open_with_graph_impact_provider_and_options(
        path_inside_repo: &Path,
        graph_impact_provider: impl GraphImpactProvider + 'static,
        options: BrokerOpenOptions,
    ) -> Result<Self, BrokerOpError> {
        let mut storage_context = BrokerStorageContext::capture(options);
        storage_context.ensure_valid()?;
        let here = GitRepo::discover(path_inside_repo)?;
        let main_root = here.main_root()?;
        let repo = GitRepo::discover(&main_root)?;
        storage_context.bind_repository(&repo)?;
        let store = BrokerStore::open_in_repo(&main_root)?;
        let mut broker = Self {
            repo,
            store,
            main_root,
            graph_impact_provider: Box::new(graph_impact_provider),
            storage_context,
            host_operation_db_path: None,
            worktree_root_override: None,
            agent_supervisor_executable: None,
            collaboration_state_override: None,
            landing_deadline: std::cell::Cell::new(None),
            main_common_dir: std::cell::OnceCell::new(),
            artifact_sweep_namespace: std::cell::OnceCell::new(),
            #[cfg(test)]
            submission_plan_test_observer: None,
            #[cfg(test)]
            worktree_lifecycle_test_observer: None,
        };
        // Fresh staged stores already have a repository-scoped bootstrap
        // identity, so a separate pinned cache root can be initialized without
        // selecting or creating a worktree root. Legacy-unbound stores defer
        // this initialization and root maintenance.
        if broker.validate_bound_repository_worktree_root()?.is_none() {
            let binding = broker.store.repository_worktree_binding()?;
            if binding.state
                == crate::store::repository_binding::RepositoryWorktreeBindingState::FreshUnbound
                && binding.origin
                    == Some(
                        crate::store::repository_binding::RepositoryWorktreeBindingOrigin::Fresh,
                    )
            {
                broker
                    .storage_context
                    .initialize_preparation_cache_root(&broker.repo)?;
            }
            // The committed collaboration capture fence is independent of
            // worktree-root binding and must also protect unbound stores.
            broker.collaboration_fence_state(FenceTrigger::Open);
            return Ok(broker);
        }
        broker
            .storage_context
            .initialize_preparation_cache_root(&broker.repo)?;
        // Fence pre-#660 binaries out of a repository whose committed config
        // requires collaboration capture. Best effort: the open never fails
        // because of it.
        broker.collaboration_fence_state(FenceTrigger::Open);
        broker.backfill_live_repository_contracts()?;
        broker.reap_abandoned_prepared_operations()?;
        broker.recover_interrupted_promotion()?;
        broker.recover_prepared_reconciliation()?;
        broker.backfill_promoted_path_exposures()?;
        // Best effort: only an already-confirmed journal may be resumed here.
        // If current eligibility or an older journal's missing fingerprint
        // prevents continuation, leave the journal for explicit review and
        // report the refusal instead of hiding it.
        crate::warn_unrecorded("resume GC maintenance", broker.resume_gc_maintenance());
        Ok(broker)
    }

    pub fn main_root(&self) -> &Path {
        &self.main_root
    }

    pub(super) fn session_context(&self, mut context: SessionContext) -> SessionContext {
        if context.repository_name.is_none()
            && (context.tab_name.is_some() || context.ai_provider.is_some())
        {
            context.repository_name = self
                .main_root
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string);
        }
        context
    }

    /// Override the per-user host-operation database.
    ///
    /// Production callers should use the platform default. This hook lets
    /// embedders and tests isolate host coordination without mutating process
    /// environment variables shared by concurrent threads.
    #[doc(hidden)]
    pub fn with_host_operation_database(mut self, path: impl Into<PathBuf>) -> Self {
        self.host_operation_db_path = Some(
            self.storage_context
                .anchor_override_path("host-operation database", path.into()),
        );
        self
    }

    /// Override the exact broker worktree root.
    ///
    /// Production callers should use host-state placement. Tests and
    /// constrained embedders can inject an isolated root without changing
    /// process-wide environment variables.
    #[doc(hidden)]
    pub fn with_worktree_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.worktree_root_override = Some(
            self.storage_context
                .anchor_override_path("worktree root", path.into()),
        );
        self
    }

    /// Select an exact executable implementing Aethyme's private supervisor protocols.
    ///
    /// CLI callers use their current executable by default. Library embedders
    /// using generic guarded exec must either forward
    /// [`crate::agent_supervisor::dispatch`] before ordinary argument parsing
    /// (it dispatches both inline-exec and agent protocols), or supply a
    /// compatible Aethyme executable here. No PATH search or installed-runtime
    /// fallback is used. The path is anchored and validated now, and revalidated
    /// before allocation.
    pub fn with_agent_supervisor_executable(
        mut self,
        path: impl AsRef<Path>,
    ) -> Result<Self, BrokerOpError> {
        self.agent_supervisor_executable = Some(
            crate::agent_supervisor::resolve_executable(Some(path.as_ref())).map_err(|source| {
                BrokerOpError::Spawn {
                    command: "agent supervisor executable".into(),
                    source,
                }
            })?,
        );
        Ok(self)
    }

    /// Keep collaboration state under `host_state` instead of the host state
    /// directory, so a test never touches the real one.
    #[doc(hidden)]
    pub fn with_collaboration_state(mut self, host_state: impl Into<PathBuf>) -> Self {
        self.collaboration_state_override = Some(
            self.storage_context
                .anchor_override_path("collaboration state directory", host_state.into()),
        );
        self
    }

    /// The collaboration fence for this repository (#660): raise
    /// `broker.db`'s compatibility floor to [`crate::COLLABORATION_FENCE_SCHEMA`]
    /// when the **committed** config requires capture, and report it.
    ///
    /// - Only the copy committed on the fetched default branch counts. An
    ///   uncommitted or experimental `required`, or a feature branch checked
    ///   out in the main checkout, never fences: the floor is never lowered.
    /// - Once fenced, this is one meta query.
    /// - Read-only stores (`query_only`: snapshot opens, the missing-database
    ///   in-memory store, migrated temporary copies) never write; a fence the
    ///   committed config requires is reported as `pending`.
    /// - Never fails: a read or write error is warned about, and a raise that
    ///   could not be written is reported as `pending`. The next writable
    ///   status, submit or promotion retries.
    pub(crate) fn collaboration_fence_state(
        &self,
        trigger: FenceTrigger<'_>,
    ) -> Option<crate::CollaborationFence> {
        let conn = self.store.connection();
        match crate::schema::collaboration_fence(conn) {
            Ok(Some(fence)) => return Some(fence),
            Ok(None) => {}
            Err(error) => {
                crate::warn_unrecorded("read the collaboration fence", Err::<(), _>(error));
                return None;
            }
        }
        let loaded;
        let config = match trigger {
            // Opens run on every hook call: reach Git only when the working
            // copy says required, then confirm against the committed copy.
            FenceTrigger::Open => {
                let text =
                    std::fs::read_to_string(self.main_root.join(".aethyme/config.toml")).ok()?;
                if !crate::collaboration_submit::requires_capture(
                    &crate::collaboration_submit::setting_from_text(Some(&text)),
                ) {
                    return None;
                }
                loaded = crate::merge::repository_config_with_source(&self.main_root);
                loaded.as_ref()
            }
            FenceTrigger::Config(config) => config,
        };
        let (text, "committed") = config.map(|(text, source)| (text.as_str(), *source))? else {
            return None;
        };
        if !crate::collaboration_submit::requires_capture(
            &crate::collaboration_submit::setting_from_text(Some(text)),
        ) {
            return None;
        }
        let read_only = conn
            .pragma_query_value(None, "query_only", |row| row.get::<_, bool>(0))
            .unwrap_or(true);
        if read_only {
            return Some(crate::CollaborationFence::pending());
        }
        match crate::schema::raise_collaboration_fence(conn, "committed")
            .and_then(|()| crate::schema::collaboration_fence(conn))
        {
            Ok(Some(fence)) => Some(fence),
            Ok(None) => Some(crate::CollaborationFence::pending()),
            Err(error) => {
                crate::warn_unrecorded(
                    "raise the collaboration fence (the next status or submit retries)",
                    Err::<(), _>(error),
                );
                Some(crate::CollaborationFence::pending())
            }
        }
    }

    /// Open `project`'s collaboration state for this repository.
    pub(crate) fn collaboration_store(
        &self,
        project: &crate::collaboration_state::ProjectKey,
    ) -> Result<
        crate::collaboration_state::CollaborationStore,
        crate::collaboration_state::CollaborationStateError,
    > {
        if let Some(root) = self.collaboration_state_override.as_deref()
            && !root.is_absolute()
        {
            return Err(
                crate::collaboration_state::CollaborationStateError::RelativeRoot {
                    root: root.to_path_buf(),
                },
            );
        }
        let root = self
            .collaboration_state_override
            .as_deref()
            .map(crate::collaboration_state::CollaborationRoot::under_host_state)
            .or_else(|| self.storage_context.collaboration_root());
        match root {
            Some(root) => crate::collaboration_state::CollaborationStore::open(
                &root,
                project,
                &crate::collaboration_state::forbidden_roots(&self.main_root),
            ),
            None if !self.storage_context.host_state_dir_is_explicit
                && crate::host_state::path_is_ephemeral(&self.main_root) =>
            {
                Err(
                    crate::collaboration_state::CollaborationStateError::EphemeralRepository {
                        repository: self.main_root.clone(),
                    },
                )
            }
            None => crate::collaboration_state::open_for_repository(&self.main_root, project),
        }
    }

    pub(crate) fn storage_context(&self) -> &BrokerStorageContext {
        &self.storage_context
    }

    pub(crate) fn worktree_volume_preparation_cache_root_bindings(
        &self,
    ) -> Result<Vec<crate::broker::PreparationCacheRootBinding>, BrokerOpError> {
        let git_common_dir =
            self.repo
                .git_common_dir()
                .map_err(|error| BrokerOpError::InvalidStorageContext {
                    reason: format!("cannot inspect repository Git common directory: {error}")
                        .into_boxed_str(),
                })?;
        Self::capture_worktree_volume_preparation_cache_roots(
            &self.main_root,
            &git_common_dir,
            &self.storage_context,
        )
    }

    pub(crate) fn preparation_cache_root_bindings_for_maintenance(
        &self,
    ) -> Result<Vec<crate::broker::PreparationCacheRootBinding>, BrokerOpError> {
        let mut bindings = self
            .storage_context
            .preparation_cache_root_binding()?
            .into_iter()
            .collect::<Vec<_>>();
        bindings.extend(self.worktree_volume_preparation_cache_root_bindings()?);
        bindings.sort_by(|left, right| left.path.cmp(&right.path));
        bindings.dedup_by(|left, right| left.path == right.path);
        Ok(bindings)
    }

    pub(crate) fn initialize_worktree_volume_preparation_cache_root(
        &self,
        worktree_path: &Path,
    ) -> Result<crate::broker::PreparationCacheRootBinding, BrokerOpError> {
        if self
            .storage_context
            .preparation_cache_root_override
            .is_some()
        {
            return Err(BrokerOpError::InvalidStorageContext {
                reason: "worktree-volume cache initialization cannot replace an explicit preparation-cache root".into(),
            });
        }
        let placement_root =
            worktree_path
                .parent()
                .ok_or_else(|| BrokerOpError::InvalidStorageContext {
                    reason: "worktree path has no placement root".into(),
                })?;
        let _resource_db_path = self
            .storage_context
            .host_resource_db_path
            .as_deref()
            .ok_or_else(|| BrokerOpError::InvalidStorageContext {
                reason: "the broker's captured host resource database path is unavailable".into(),
            })?;
        let placement_marker = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: self.repository_worktree_key()?,
            repository_root: self.main_root.clone(),
        };
        let placement_guard =
            capture_worktree_placement_root_guard(placement_root, &placement_marker)?;
        crate::preparation::rotation_debt::ensure_preparation_cache_root_child(
            placement_root,
            WORKTREE_VOLUME_PREPARATION_CACHE_ROOT,
            &placement_guard.identity,
        )
        .map_err(|error| BrokerOpError::InvalidStorageContext {
            reason: format!(
                "cannot create or capture worktree-volume preparation-cache root under {}: {error}",
                placement_root.display()
            )
            .into_boxed_str(),
        })?;
        let cache_root = placement_root.join(WORKTREE_VOLUME_PREPARATION_CACHE_ROOT);
        let (canonical, identity, state, directory) = capture_preparation_cache_root(&cache_root)
            .map_err(|reason| {
            BrokerOpError::InvalidStorageContext {
                reason: format!(
                    "cannot capture worktree-volume preparation-cache root {}: {reason}",
                    cache_root.display()
                )
                .into_boxed_str(),
            }
        })?;
        let placement_identity = crate::preparation::rotation_debt::DirectoryIdentity::capture(
            placement_root,
        )
        .map_err(|error| BrokerOpError::InvalidStorageContext {
            reason: format!(
                "cannot verify worktree placement filesystem at {}: {error}",
                placement_root.display()
            )
            .into_boxed_str(),
        })?;
        let cache_root_identity = crate::preparation::rotation_debt::DirectoryIdentity::capture(
            &canonical,
        )
        .map_err(|error| BrokerOpError::InvalidStorageContext {
            reason: format!(
                "cannot verify worktree-volume preparation-cache filesystem at {}: {error}",
                canonical.display()
            )
            .into_boxed_str(),
        })?;
        if !placement_identity.same_filesystem(&cache_root_identity) {
            return Err(BrokerOpError::InvalidStorageContext {
                reason: "worktree-volume preparation-cache root is on a different filesystem from the worktree placement root".into(),
            });
        }
        validate_worktree_placement_root_guard(&placement_guard)?;
        let mut cache_context = self.storage_context.clone();
        cache_context.preparation_cache_root_override = Some(canonical);
        cache_context.preparation_cache_root_identity = Some(identity);
        cache_context.preparation_cache_root_state = Some(state);
        cache_context.preparation_cache_root_handle = Some(directory);
        cache_context.initialize_preparation_cache_root(&self.repo)?;
        validate_worktree_placement_root_guard(&placement_guard)?;
        let mut binding = cache_context
            .preparation_cache_root_binding()?
            .ok_or_else(|| BrokerOpError::InvalidStorageContext {
                reason:
                    "initialized worktree-volume cache root has no captured host-resource database"
                        .into(),
            })?;
        binding
            .root_guard
            .as_mut()
            .ok_or_else(|| BrokerOpError::InvalidStorageContext {
                reason: "initialized worktree-volume cache root has no retained guard".into(),
            })?
            .placement_root = Some(placement_guard);
        binding.validate()?;
        let worktree_identity = crate::preparation::rotation_debt::DirectoryIdentity::capture(
            worktree_path,
        )
        .map_err(|error| BrokerOpError::InvalidStorageContext {
            reason: format!(
                "cannot verify worktree filesystem at {}: {error}",
                worktree_path.display()
            )
            .into_boxed_str(),
        })?;
        let namespace_identity = crate::preparation::rotation_debt::DirectoryIdentity::capture(
            &binding.path,
        )
        .map_err(|error| BrokerOpError::InvalidStorageContext {
            reason: format!(
                "cannot verify preparation-cache namespace filesystem at {}: {error}",
                binding.path.display()
            )
            .into_boxed_str(),
        })?;
        if !worktree_identity.same_filesystem(&namespace_identity) {
            return Err(BrokerOpError::InvalidStorageContext {
                reason: "worktree-volume preparation-cache root is on a different filesystem from the worktree".into(),
            });
        }
        Ok(binding)
    }

    /// Discover only already-present worktree-volume cache roots beneath the
    /// repository's captured, marker-bound placement candidates. Missing children
    /// are ordinary absence; an existing child that cannot be proved belongs to
    /// this repository makes the maintenance inventory incomplete.
    pub(crate) fn capture_worktree_volume_preparation_cache_roots(
        main_root: &Path,
        git_common_dir: &Path,
        context: &BrokerStorageContext,
    ) -> Result<Vec<crate::broker::PreparationCacheRootBinding>, BrokerOpError> {
        let plan = plan_worktree_root_with_context(main_root, Some(git_common_dir), context);
        if let Some(reason) = &plan.storage_context_refusal {
            return Err(BrokerOpError::InvalidStorageContext {
                reason: reason.clone().into_boxed_str(),
            });
        }
        let marker = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: plan.repository_key.clone(),
            repository_root: main_root.to_path_buf(),
        };
        let mut bindings = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for (placement_root, _) in worktree_root_candidates_for(&plan) {
            let cache_root = placement_root.join(WORKTREE_VOLUME_PREPARATION_CACHE_ROOT);
            match std::fs::symlink_metadata(&cache_root) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(BrokerOpError::InvalidStorageContext {
                        reason: format!(
                            "cannot inspect worktree-volume preparation-cache root {}: {error}",
                            cache_root.display()
                        )
                        .into_boxed_str(),
                    });
                }
                Ok(_) => {}
            }
            let Some(resource_db_path) = context.host_resource_db_path.as_deref() else {
                return Err(BrokerOpError::InvalidStorageContext {
                reason: format!(
                    "the broker's captured host-resource database path is unavailable for existing worktree-volume cache root {}",
                    cache_root.display()
                )
                .into_boxed_str(),
            });
            };
            let Some(binding) = capture_worktree_volume_preparation_cache_root(
                &placement_root,
                &marker,
                resource_db_path,
            )?
            else {
                continue;
            };
            binding.validate()?;
            if seen.insert(binding.path.clone()) {
                bindings.push(binding);
            }
        }
        bindings.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(bindings)
    }

    /// Bind a child command to the storage context captured by this broker.
    ///
    /// Call this before spawning a child that will operate on the same
    /// repository. The child receives a versioned, repository-bound context
    /// and rejects it if it cannot validate that context; this method does not
    /// grant permissions or create a child process.
    pub fn configure_storage_context_for_child(
        &self,
        command: &mut Command,
    ) -> Result<(), BrokerOpError> {
        self.storage_context.ensure_valid()?;
        set_command_path(
            command,
            "AETHYME_HOST_STATE_DIR",
            self.storage_context.host_state_dir.as_deref(),
        );
        set_command_path(
            command,
            "AETHYME_HOST_CACHE_DIR",
            self.storage_context.host_cache_dir.as_deref(),
        );
        set_command_path(
            command,
            PREPARATION_CACHE_ROOT_ENV,
            self.storage_context
                .preparation_cache_root_override
                .as_deref(),
        );
        let broker_database_path = self.store.db_path();
        if !broker_database_path.is_absolute() {
            return Err(BrokerOpError::InvalidStorageContext {
                reason: "broker database path for a child must be absolute".into(),
            });
        }
        set_command_path(command, "AETHYME_BROKER_DB", Some(broker_database_path));
        command.env(
            BROKER_OPEN_HOST_STATE_EXPLICIT_ENV,
            if self.storage_context.host_state_dir_is_explicit {
                "true"
            } else {
                "false"
            },
        );

        let mut child_context = self.storage_context.clone();
        child_context.host_operation_db_path = self
            .host_operation_db_path
            .clone()
            .or(child_context.host_operation_db_path.clone());
        set_command_path(
            command,
            BROKER_OPEN_HOST_OPERATION_DB_ENV,
            child_context.host_operation_db_path.as_deref(),
        );

        if let Some(path) = &self.worktree_root_override {
            command.env(BROKER_OPEN_WORKTREE_ROOT_ENV, path);
            command.env_remove("AETHYME_WORKTREE_ROOT");
            child_context.worktree_root_override = Some(path.clone());
            child_context.worktree_root_environment_override = None;
        } else if let Some(path) = &self.storage_context.worktree_root_override {
            command.env(BROKER_OPEN_WORKTREE_ROOT_ENV, path);
            command.env_remove("AETHYME_WORKTREE_ROOT");
            child_context.worktree_root_override = Some(path.clone());
            child_context.worktree_root_environment_override = None;
        } else {
            command.env_remove(BROKER_OPEN_WORKTREE_ROOT_ENV);
            set_command_path(
                command,
                "AETHYME_WORKTREE_ROOT",
                self.storage_context
                    .worktree_root_environment_override
                    .as_deref(),
            );
            child_context.worktree_root_override = None;
        }

        let collaboration_root = self
            .collaboration_state_override
            .as_deref()
            .map(|host_state| host_state.join("collaboration"))
            .or_else(|| self.storage_context.collaboration_state_dir.clone())
            .or_else(|| {
                (self.storage_context.host_state_dir_is_explicit)
                    .then(|| {
                        self.storage_context
                            .host_state_dir
                            .as_ref()
                            .map(|path| path.join("collaboration"))
                    })
                    .flatten()
            });
        set_command_path(
            command,
            BROKER_OPEN_COLLABORATION_ROOT_ENV,
            collaboration_root.as_deref(),
        );
        child_context.collaboration_state_dir = collaboration_root;
        let encoded = if child_context.preparation_cache_root_override.is_some() {
            CapturedBrokerStorageContextV4::encode(&child_context)?
        } else {
            CapturedBrokerStorageContextV2::encode(&child_context)?
        };
        command.env(BROKER_OPEN_STORAGE_CONTEXT_ENV, encoded);
        Ok(())
    }

    pub(crate) fn host_operation_database_path(&self) -> Result<PathBuf, BrokerOpError> {
        self.storage_context.ensure_valid()?;
        let path = self
            .host_operation_db_path
            .clone()
            .or_else(|| self.storage_context.host_operation_db_path.clone())
            .ok_or_else(|| crate::HostOperationError::StateDirectoryUnavailable)?;
        if !path.is_absolute() {
            return Err(BrokerOpError::InvalidStorageContext {
                reason: "host-operation database path must be absolute".into(),
            });
        }
        Ok(path)
    }

    pub fn store(&mut self) -> &mut BrokerStore {
        &mut self.store
    }

    pub(crate) fn store_ref(&self) -> &BrokerStore {
        &self.store
    }

    /// Build a bounded, allowlist-only lease routing snapshot without
    /// refreshing leases, events, sessions, or command telemetry.
    pub fn export_lease_routing(
        &self,
        options: crate::LeaseRoutingExportOptions,
        source_time: i64,
    ) -> Result<crate::LeaseRoutingExport, crate::LeaseRoutingExportError> {
        crate::lease_export::build_lease_routing_export(
            &self.repo,
            &self.store,
            options,
            source_time,
        )
    }

    /// Persist an immutable non-blocking advisory and refresh the generated
    /// outstanding-advisory projection from authoritative database state.
    pub fn persist_advisory(
        &mut self,
        mut advisory: NewAdvisory,
    ) -> Result<Advisory, BrokerOpError> {
        let identity = advisory.identity.trim();
        if identity.is_empty() || identity.len() > 256 {
            return Err(BrokerOpError::InvalidAdvisory {
                reason: "identity must contain 1..=256 bytes".into(),
            });
        }
        advisory.identity = identity.to_string();
        if advisory.paths.len() > 100 || advisory.evidence.len() > 100 {
            return Err(BrokerOpError::InvalidAdvisory {
                reason: "paths and evidence are each limited to 100 entries".into(),
            });
        }
        advisory.paths = advisory
            .paths
            .iter()
            .map(|path| {
                if path.len() > 4_096 {
                    return Err(BrokerOpError::InvalidAdvisory {
                        reason: "each advisory path is limited to 4096 bytes".into(),
                    });
                }
                normalize_lease_path(path)
            })
            .collect::<Result<Vec<_>, _>>()?;
        advisory.paths.sort();
        advisory.paths.dedup();
        for evidence in &advisory.evidence {
            if evidence.kind.trim().is_empty()
                || evidence.kind.len() > 128
                || evidence.summary.trim().is_empty()
                || evidence.summary.len() > 4_096
            {
                return Err(BrokerOpError::InvalidAdvisory {
                    reason: "evidence kind must contain 1..=128 bytes and summary 1..=4096 bytes"
                        .into(),
                });
            }
        }
        if let Some(sha) = &advisory.integration_sha
            && (sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(BrokerOpError::InvalidAdvisory {
                reason: "integration SHA must be a full 40-character hexadecimal object id".into(),
            });
        }
        if let Some(session_id) = advisory.session_id {
            self.store.session(session_id)?;
        }
        if let Some(queue_entry_id) = advisory.queue_entry_id {
            let queue = self.store.merge_queue()?;
            queue
                .iter()
                .find(|entry| entry.id == queue_entry_id)
                .ok_or_else(|| BrokerOpError::InvalidAdvisory {
                    reason: format!("queue entry {queue_entry_id} does not exist"),
                })?;
        }
        let stored = self.store.record_advisory(&advisory)?;
        self.refresh_advisory_projection()?;
        Ok(stored)
    }

    pub fn advisories(&self, include_all: bool) -> Result<Vec<Advisory>, BrokerOpError> {
        Ok(self.store.advisories(include_all)?)
    }

    /// Derive current maintainer recommendations from a bounded, redacted
    /// history window without mutating advisory or repository state.
    pub fn maintainer_recommendations(
        &self,
    ) -> Result<Vec<crate::MaintainerRecommendation>, BrokerOpError> {
        Ok(crate::recommendations::recommendations_from_store(
            &self.store,
        )?)
    }

    /// Synchronize the explicit maintainer advisory inventory from bounded
    /// history. This is used only by deliberate advisory commands; readiness
    /// and gate doctor keep using the read-only projection above.
    pub fn refresh_maintainer_recommendations(&mut self) -> Result<(), BrokerOpError> {
        let snapshot = crate::recommendations::recommendation_snapshot_from_store(&self.store)?;
        Ok(self.store.sync_maintainer_recommendations(&snapshot)?)
    }

    pub fn advisory_list(&self, include_acknowledged: bool) -> Result<AdvisoryList, BrokerOpError> {
        let advisories = self.store.advisories(include_acknowledged)?;
        let outstanding_count = advisories
            .iter()
            .filter(|advisory| {
                advisory.resolution_state == crate::AdvisoryResolutionState::Outstanding
            })
            .count();
        Ok(AdvisoryList {
            advisories,
            outstanding_count,
            includes_acknowledged: include_acknowledged,
        })
    }

    pub fn advisory(&self, id: i64) -> Result<Advisory, BrokerOpError> {
        self.store
            .advisory(id)?
            .ok_or_else(|| BrokerError::AdvisoryNotFound(id).into())
    }

    pub fn record_advisories_shown(
        &mut self,
        advisories: &[Advisory],
        surface: crate::AdvisoryDeliverySurface,
    ) -> Result<(), BrokerOpError> {
        Ok(self.store.record_advisories_shown(advisories, surface)?)
    }

    pub fn advisory_delivery_metrics(
        &self,
    ) -> Result<Vec<crate::AdvisoryDeliveryMetric>, BrokerOpError> {
        Ok(self.store.advisory_delivery_metrics()?)
    }

    pub fn advisory_delivery_summary(
        &self,
    ) -> Result<crate::AdvisoryDeliverySummary, BrokerOpError> {
        Ok(self.store.advisory_delivery_summary()?)
    }

    pub fn acknowledge_advisory(&mut self, id: i64) -> Result<Advisory, BrokerOpError> {
        let advisory = self.store.acknowledge_advisory(id)?;
        self.refresh_advisory_projection()?;
        Ok(advisory)
    }

    pub fn suppress_maintainer_advisory(&mut self, id: i64) -> Result<Advisory, BrokerOpError> {
        Ok(self.store.suppress_maintainer_advisory(id)?)
    }

    pub fn send_session_note(
        &mut self,
        sender_session_id: i64,
        recipient_session_id: i64,
        message: &str,
    ) -> Result<SessionNote, BrokerOpError> {
        if sender_session_id == recipient_session_id {
            return Err(BrokerOpError::InvalidSessionNote {
                reason: "sender and recipient must be different live sessions".into(),
            });
        }
        let message = message.trim();
        if message.is_empty() {
            return Err(BrokerOpError::InvalidSessionNote {
                reason: "message must not be empty".into(),
            });
        }
        if message.len() > SESSION_NOTE_MAX_BYTES {
            return Err(BrokerOpError::InvalidSessionNote {
                reason: format!(
                    "message is {} bytes; maximum is {SESSION_NOTE_MAX_BYTES}",
                    message.len()
                ),
            });
        }
        if message.chars().any(char::is_control) {
            return Err(BrokerOpError::InvalidSessionNote {
                reason: "message must be one line of plain text without control characters".into(),
            });
        }
        for (role, id) in [
            ("sender", sender_session_id),
            ("recipient", recipient_session_id),
        ] {
            let session = self.store.session(id)?;
            if session.status.is_closed() {
                return Err(BrokerOpError::InvalidSessionNote {
                    reason: format!("{role} session {id} is closed"),
                });
            }
        }
        Ok(self
            .store
            .record_session_note(sender_session_id, recipient_session_id, message)?)
    }

    pub fn session_note_list(
        &self,
        recipient_session_id: i64,
    ) -> Result<SessionNoteList, BrokerOpError> {
        self.store.session(recipient_session_id)?;
        let notes = self.store.session_notes(recipient_session_id)?;
        let unread_count = notes
            .iter()
            .filter(|note| note.acknowledged_at.is_none())
            .count();
        Ok(SessionNoteList {
            notes,
            unread_count,
        })
    }

    pub fn acknowledge_session_note(
        &mut self,
        recipient_session_id: i64,
        note_id: i64,
    ) -> Result<SessionNote, BrokerOpError> {
        let recipient = self.store.session(recipient_session_id)?;
        if recipient.status.is_closed() {
            return Err(BrokerOpError::InvalidSessionNote {
                reason: format!("recipient session {recipient_session_id} is closed"),
            });
        }
        let note = self
            .store
            .session_note(note_id)?
            .ok_or(BrokerError::SessionNoteNotFound(note_id))?;
        if note.recipient_session_id != recipient_session_id {
            return Err(BrokerOpError::SessionNoteRecipientMismatch {
                session_id: recipient_session_id,
                note_id,
                recipient_session_id: note.recipient_session_id,
            });
        }
        Ok(self.store.acknowledge_session_note(note_id)?)
    }

    pub fn refresh_advisory_projection(&mut self) -> Result<PathBuf, BrokerOpError> {
        let main_root = self.main_root.clone();
        crate::advisories::project(&main_root, || {
            Ok(self
                .store
                .advisories(false)?
                .into_iter()
                .filter(|advisory| advisory.audience == crate::AdvisoryAudience::Session)
                .collect())
        })
    }

    pub(crate) fn main_root_path(&self) -> PathBuf {
        self.main_root.clone()
    }

    pub(crate) fn repo_handle(&self) -> &GitRepo {
        &self.repo
    }

    pub(super) fn capture_repository_contract(
        &self,
        checkout_root: &Path,
        backfilled: bool,
    ) -> Result<crate::RepositoryContract, BrokerOpError> {
        // Canonical deployment files travel with each worktree. Local-only
        // artifacts are intentionally untracked and belong to the clone's
        // primary checkout, so a spawned linked worktree cannot contain them.
        let contract_root = if crate::detect_repository_mode(&self.main_root)
            == crate::RepositoryDeploymentMode::LocalOnly
        {
            self.main_root.as_path()
        } else {
            checkout_root
        };
        crate::RepositoryContract::capture(contract_root, backfilled).map_err(|reason| {
            BrokerOpError::RepositoryContract {
                path: contract_root.display().to_string(),
                reason,
            }
        })
    }

    pub(super) fn backfill_live_repository_contracts(&mut self) -> Result<(), BrokerOpError> {
        for session in self.store.sessions_missing_repository_contract()? {
            let recorded_worktree = PathBuf::from(&session.worktree_path);
            let checkout_root = if recorded_worktree.is_dir() {
                recorded_worktree.as_path()
            } else {
                self.main_root.as_path()
            };
            let contract = self.capture_repository_contract(checkout_root, true)?;
            self.store
                .backfill_session_repository_contract(session.id, &contract)?;
        }
        Ok(())
    }

    pub fn pr_check(
        &mut self,
        options: crate::PrCheckOptions,
    ) -> Result<crate::PrCheckReport, BrokerOpError> {
        crate::pr::check_pr_followup(self, options)
    }

    // ── adopt (attach-first) ──────────────────────────────────────────

    /// Register an existing worktree the user already launched an agent
    /// in. `worktree` may be any path inside it.
    pub fn adopt(&mut self, worktree: &Path, task: Option<&str>) -> Result<Session, BrokerOpError> {
        Ok(self
            .adopt_with(worktree, task, AdoptMode::New, None)?
            .session)
    }

    /// `adopt` with an explicit policy for the "this worktree already has
    /// a session" case (dogfood feedback 2026-07-14: the bare constraint
    /// error left no obvious follow-up path).
    ///
    /// `agent_identity` is the `Name <email>` the session's agent is
    /// credited under on the promote commit; `None` leaves it unknown.
    pub fn adopt_with(
        &mut self,
        worktree: &Path,
        task: Option<&str>,
        mode: AdoptMode,
        agent_identity: Option<&str>,
    ) -> Result<AdoptReport, BrokerOpError> {
        self.adopt_with_options(worktree, task, AdoptOptions::new(mode), agent_identity)
    }

    pub fn adopt_with_options(
        &mut self,
        worktree: &Path,
        task: Option<&str>,
        options: AdoptOptions,
        agent_identity: Option<&str>,
    ) -> Result<AdoptReport, BrokerOpError> {
        self.adopt_with_options_and_context(
            worktree,
            task,
            options,
            agent_identity,
            SessionContext::default(),
        )
    }

    pub fn adopt_with_options_and_context(
        &mut self,
        worktree: &Path,
        task: Option<&str>,
        options: AdoptOptions,
        agent_identity: Option<&str>,
        context: SessionContext,
    ) -> Result<AdoptReport, BrokerOpError> {
        // For an existing session, keep the established bounded default
        // refresh and comparison, but do it before task/liveness changes.
        let prefetched_default_branch = if options.mode == AdoptMode::Reuse {
            let checkout = GitRepo::discover(worktree)?;
            let worktree_path = checkout.root().to_string_lossy().into_owned();
            if let Some(session) = self.store.session_for_worktree(&worktree_path)? {
                let (drift, note, refresh) = self.reused_session_drift(&session);
                Some(ReusePrefetchedDefaultBranch {
                    session_id: session.id,
                    drift,
                    note,
                    refresh,
                })
            } else {
                None
            }
        } else {
            None
        };
        let mut report = self.adopt_with_options_and_context_unrefreshed(
            worktree,
            task,
            options,
            agent_identity,
            context,
            prefetched_default_branch,
        )?;
        // A --reuse request that created a new adopted row had no session
        // to preflight. Preserve the existing drift report for that path.
        if report.outcome != AdoptOutcome::Reused {
            let (drift, note, _) = self.reused_session_drift(&report.session);
            report.default_branch = drift;
            report.default_branch_note = note;
        }
        Ok(report)
    }

    fn adopt_with_options_and_context_unrefreshed(
        &mut self,
        worktree: &Path,
        task: Option<&str>,
        options: AdoptOptions,
        agent_identity: Option<&str>,
        context: SessionContext,
        prefetched_default_branch: Option<ReusePrefetchedDefaultBranch>,
    ) -> Result<AdoptReport, BrokerOpError> {
        let context = self.session_context(context);
        if options.sync_integration && options.mode != AdoptMode::Reuse {
            return Err(BrokerOpError::ReuseSyncRequiresReuse);
        }
        // All ownership changes in this repository share one cross-process
        // claim. Rediscover after acquiring it because cleanup may have
        // removed or replaced this checkout while this caller was waiting.
        let observed_checkout = GitRepo::discover(worktree)?;
        let observed_root = observed_checkout.root().to_path_buf();
        let _lifecycle_claim = self.claim_worktree_lifecycle()?;
        let checkout = GitRepo::discover(worktree)?;
        if checkout.root() != observed_root.as_path() {
            return Err(GitError::UntrustedOutput {
                detail: "worktree path changed while waiting for its lifecycle claim".into(),
            }
            .into());
        }
        let branch = checkout.current_branch()?;
        let worktree_path = checkout.root().to_string_lossy().into_owned();
        let existing = self.store.session_for_worktree(&worktree_path)?;
        if options.mode == AdoptMode::New
            && let Some(existing) = &existing
        {
            return Err(BrokerOpError::SessionExistsForWorktree {
                id: existing.id,
                status: existing.status.as_str(),
                task: existing
                    .task
                    .as_deref()
                    .map(|task| format!(", task: {task:?}"))
                    .unwrap_or_default(),
            });
        }
        let planned_paths = normalize_planned_paths(&options.planned_paths)?;
        let plan_owner = existing.as_ref().and_then(|session| match options.mode {
            AdoptMode::Reuse | AdoptMode::ReplaceStale => Some(session.id),
            AdoptMode::New => None,
        });
        self.ensure_planned_paths_available(&planned_paths, plan_owner)?;
        let mut integration_sync = None;
        let mut outcome = AdoptOutcome::Created;
        let mut replaced_session_id = None;

        if let Some(existing) = existing {
            match options.mode {
                AdoptMode::Reuse => {
                    let prefetched = prefetched_default_branch
                        .as_ref()
                        .filter(|prefetched| prefetched.session_id == existing.id)
                        .ok_or_else(|| BrokerOpError::SessionSyncRefused {
                            reason: "the session changed while reuse freshness was being prepared; retry the task handoff".into(),
                        })?;
                    let freshness = self.revalidate_reuse_base(
                        &checkout,
                        &existing,
                        options.sync_integration,
                        prefetched,
                    )?;
                    integration_sync = freshness.integration_sync.clone();
                    let refreshed_base = integration_sync
                        .as_ref()
                        .map(|sync| sync.after_head.as_str())
                        .or(freshness.refresh_diff_base.as_deref());
                    let checkout_moved_to = freshness.checkout_moved_to.as_deref();
                    let foreign_files = checkout.untracked_paths().map_err(|error| {
                        match checkout_moved_to {
                            Some(commit) => reuse_freshness_refusal(format!(
                                "the checkout was advanced to {commit}, but foreign-file discovery failed ({error}); retry reuse after inspecting the checkout"
                            )),
                            None => error.into(),
                        }
                    })?;
                    let integration_drift = Some(
                        self.adopt_integration_drift(&checkout, existing.id, false)
                            .map_err(|error| match checkout_moved_to {
                                Some(commit) => reuse_freshness_refusal(format!(
                                    "the checkout was advanced to {commit}, but the integration report could not be completed ({error}); retry reuse after inspecting the checkout"
                                )),
                                None => error,
                            })?,
                    );
                    let session_result = self
                        .store
                        .reuse_session_with_context_and_leases_and_base_observation(
                            crate::store::SessionReuseOptions {
                                verify_only: LeaseRefusalPolicy::verify_only_at(&self.main_root),
                                id: existing.id,
                                task,
                                diff_base: refreshed_base,
                                agent_identity,
                                context: &context,
                                planned_paths: &planned_paths,
                                base_observation: Some(&freshness.observation),
                            },
                        );
                    let session = session_result.map_err(|error| match checkout_moved_to {
                        Some(commit) => reuse_freshness_refusal(format!(
                            "the checkout was advanced to {commit}, but the session/task update was not persisted ({error}); the task and base record remain unchanged, so retry reuse after inspecting the checkout"
                        )),
                        None => error.into(),
                    })?;
                    self.store
                        .set_session_foreign_files(session.id, &foreign_files)
                        .map_err(|error| {
                            let state = checkout_moved_to
                                .map(|commit| format!("the checkout and session/task update reached {commit}"))
                                .unwrap_or_else(|| "the session/task update succeeded".into());
                            reuse_freshness_refusal(format!(
                                "{state}, but foreign-file bookkeeping failed ({error}); retry reuse to refresh it"
                            ))
                        })?;
                    let planned_explicit_leases = self
                        .planned_explicit_leases(session.id, &planned_paths)
                        .map_err(|error| {
                            reuse_freshness_refusal(format!(
                                "reuse updated the session task, but lease reporting failed ({error}); retry reuse to refresh the report"
                            ))
                        })?;
                    let preparation = self.preparation_status(session.id).map_err(|error| {
                        reuse_freshness_refusal(format!(
                            "reuse updated the session task, but preparation status could not be read ({error}); retry reuse to refresh the report"
                        ))
                    })?;
                    let (default_branch, default_branch_note) = self
                        .default_branch_after_reuse(&checkout, &session, prefetched, &freshness);
                    return Ok(AdoptReport {
                        session,
                        outcome: AdoptOutcome::Reused,
                        integration_drift,
                        integration_sync,
                        base_freshness: Some(freshness.report),
                        planned_explicit_leases,
                        preparation,
                        renamed_targets: Vec::new(),
                        default_branch,
                        default_branch_note,
                        carried_ownership: None,
                    });
                }
                AdoptMode::ReplaceStale => {
                    outcome = AdoptOutcome::Replaced;
                    replaced_session_id = Some(existing.id);
                }
                AdoptMode::New => {
                    return Err(BrokerOpError::SessionExistsForWorktree {
                        id: existing.id,
                        status: existing.status.as_str(),
                        task: existing
                            .task
                            .as_deref()
                            .map(|t| format!(", task: {t:?}"))
                            .unwrap_or_default(),
                    });
                }
            }
        }

        // Non-reuse adoption does not move the checkout. Capture its ownership
        // and foreign-file state only after the reuse branch has completed, so
        // a reused checkout that was fast-forwarded never records stale data.
        let diff_base = checkout.head_commit().ok();
        let repository_contract = self.capture_repository_contract(checkout.root(), false)?;
        let foreign_files = checkout.untracked_paths()?;

        // Issue #294: the same protection reuse gives a live session's
        // baseline, for the session this adoption succeeds. Without it a
        // close followed by adopt silently disowned every unsubmitted commit.
        let predecessor = match replaced_session_id {
            Some(id) => Some(self.store.session(id)?),
            None => self.store.closed_session_for_worktree(&worktree_path)?,
        };
        let carried_ownership = match (predecessor.as_ref(), diff_base.as_deref()) {
            (Some(previous), Some(head)) if previous.branch == branch => {
                self.carried_ownership(previous, head)
            }
            _ => None,
        };
        let ownership_base = carried_ownership
            .as_ref()
            .map(|carried| carried.baseline.clone())
            .or_else(|| diff_base.clone());
        let adopted_base_observation =
            ownership_base
                .clone()
                .or_else(|| diff_base.clone())
                .map(|target_commit| crate::broker::SessionBaseObservation {
                    schema_version: 1,
                    target_ref: "adopted_baseline".into(),
                    target_commit,
                    evidence: None,
                    observed_at_ms: Some(crate::clock::epoch_ms()),
                    promote_branch: crate::merge::PromoteConfig::load(&self.main_root).branch,
                    default_ref: None,
                });
        let new_session = NewSession {
            worktree_path,
            branch,
            origin: SessionOrigin::Adopted,
            task: task.map(str::to_string),
            adoption_base: ownership_base.clone(),
            adopted_head: diff_base.clone(),
            diff_base: ownership_base,
            repository_contract: Some(repository_contract),
            pid: None,
            command: None,
            log_path: None,
            agent_identity: agent_identity.map(str::to_string),
        };
        let session = match (replaced_session_id, adopted_base_observation.as_ref()) {
            (Some(replaced_session_id), Some(observation)) => self
                .store
                .replace_session_with_context_and_leases_and_base_observation(
                    crate::store::SessionReplacementOptions {
                        verify_only: LeaseRefusalPolicy::verify_only_at(&self.main_root),
                        replaced_id: replaced_session_id,
                        new: &new_session,
                        context: &context,
                        planned_paths: &planned_paths,
                        base_observation: Some(observation),
                    },
                )?,
            (Some(replaced_session_id), None) => {
                self.store.replace_session_with_context_and_leases(
                    LeaseRefusalPolicy::verify_only_at(&self.main_root),
                    replaced_session_id,
                    &new_session,
                    &context,
                    &planned_paths,
                )?
            }
            (None, Some(observation)) => self
                .store
                .register_session_with_context_and_leases_and_base_observation(
                    LeaseRefusalPolicy::verify_only_at(&self.main_root),
                    &new_session,
                    &context,
                    &planned_paths,
                    observation,
                )?,
            (None, None) => self.store.register_session_with_context_and_leases(
                LeaseRefusalPolicy::verify_only_at(&self.main_root),
                &new_session,
                &context,
                &planned_paths,
            )?,
        };
        self.store
            .set_session_foreign_files(session.id, &foreign_files)?;
        // Computed for every adopt, not only reuse. Adopting a worktree that
        // already has commits records the current HEAD as the baseline, so those
        // commits are not session-owned and submit will not replay them. Saying
        // nothing here is what turns that into a JSON archaeology exercise at
        // submit time (issue #144).
        let integration_drift = Some(self.adopt_integration_drift(
            &checkout,
            session.id,
            options.mode == AdoptMode::Reuse,
        )?);
        let planned_explicit_leases = self.planned_explicit_leases(session.id, &planned_paths)?;
        let preparation = self.preparation_status(session.id)?;
        // Reported here because this is before a replay is attempted; failing
        // later looks like a plain modify/delete conflict (issue #145).
        let renamed_targets = self.session_renamed_targets(session.id).unwrap_or_default();
        Ok(AdoptReport {
            session,
            outcome,
            integration_drift,
            integration_sync,
            base_freshness: None,
            planned_explicit_leases,
            preparation,
            renamed_targets,
            default_branch: None,
            default_branch_note: None,
            carried_ownership,
        })
    }

    /// The previous session's ownership boundary, when it still owns commits
    /// at `head` that integration does not have yet. `None` leaves the new
    /// session's baseline at `head`, as before.
    pub(super) fn carried_ownership(
        &self,
        previous: &Session,
        head: &str,
    ) -> Option<AdoptCarriedOwnership> {
        let (_, integration_head) = self.integration_head_snapshot().ok()?;
        let plan = self
            .build_submission_plan(previous, head, &integration_head)
            .ok()?;
        let pending_owned_commits = plan.pending_owned_commit_ids().len();
        if pending_owned_commits == 0 {
            return None;
        }
        Some(AdoptCarriedOwnership {
            from_session: previous.id,
            baseline: plan.recorded_baseline?,
            pending_owned_commits,
        })
    }

    fn revalidate_reuse_base(
        &mut self,
        checkout: &GitRepo,
        session: &Session,
        sync_integration: bool,
        prefetched: &ReusePrefetchedDefaultBranch,
    ) -> Result<ReuseFreshnessDecision, BrokerOpError> {
        if prefetched.session_id != session.id {
            return Err(reuse_freshness_refusal(
                "the session changed while its base was being checked; retry the task handoff",
            ));
        }

        let policy = crate::worktree_freshness::WorktreeFreshnessPolicy::load(&self.main_root)
            .map_err(reuse_freshness_refusal)?;
        let raw_observation = self
            .store
            .meta_get(&crate::broker::SessionBaseObservation::meta_key(session.id))?;
        let previous = raw_observation
            .and_then(|raw| {
                serde_json::from_str::<crate::broker::SessionBaseObservation>(&raw).ok()
            })
            .filter(|observation| {
                observation.schema_version == 1
                    && !observation.target_ref.trim().is_empty()
                    && is_full_commit_oid(&observation.target_commit)
            });
        let previous_observed_at = previous
            .as_ref()
            .and_then(|observation| observation.observed_at_ms);
        let now = crate::clock::epoch_ms();
        let previous_observation_age = crate::worktree_freshness::classify_prior_observation(
            previous_observed_at,
            now,
            policy.max_age_seconds,
        );

        // Require remote confirmation before any checkout movement or task
        // update. A pinned explicit base is intentionally independent of the
        // remote, unless the caller explicitly selects integration instead.
        let pinned = !sync_integration
            && previous.as_ref().is_some_and(|observation| {
                observation.evidence == Some(crate::SessionStartBaseEvidence::ExplicitBase)
            });
        let expected_default = crate::session_push::tracked_default(&self.repo)
            .map(|default| format!("{}/{}", default.remote, default.branch))
            .or_else(|| {
                previous
                    .as_ref()
                    .and_then(|observation| observation.default_ref.clone())
            });
        let confirmed_default = if pinned {
            None
        } else if let Some(display_ref) = expected_default {
            Some(self.require_fresh_default(prefetched, &display_ref)?)
        } else if self.repo.has_configured_remote_default().map_err(|error| {
            reuse_freshness_refusal(format!(
                "could not verify remote-default configuration: {error}"
            ))
        })? {
            return Err(reuse_freshness_refusal(
                "the configured remote default ref is unavailable; fetch the default branch and repair its tracking configuration before retrying",
            ));
        } else {
            None
        };

        let promote = crate::PromoteConfig::load(&self.main_root);
        let mut integration_sync = None;
        let target = if sync_integration {
            let integration_ref = format!("refs/heads/{}", promote.branch);
            let integration_head = self.repo.resolve_ref(&integration_ref).ok_or_else(|| {
                reuse_freshness_refusal(format!(
                    "configured integration ref {integration_ref} is missing; it was not created. Restore it or choose an explicit session base"
                ))
            })?;
            self.require_base_contains_default(&integration_head, confirmed_default.as_deref())?;
            let sync = self.synchronize_reuse_checkout(
                checkout,
                &promote.branch,
                &integration_head,
                &session.branch,
            )?;
            let default_ref = prefetched
                .refresh
                .as_ref()
                .map(|refresh| refresh.display_ref.clone())
                .or_else(|| {
                    previous
                        .as_ref()
                        .and_then(|observation| observation.default_ref.clone())
                });
            let target = ReuseTarget {
                reference: integration_ref,
                commit: sync.integration_head.clone(),
                evidence: Some(crate::SessionStartBaseEvidence::IntegrationTip),
                default_ref,
                promote_branch: promote.branch.clone(),
                verify_ref: true,
            };
            integration_sync = Some(sync);
            target
        } else if let Some(previous) = previous.as_ref() {
            match previous.evidence {
                Some(evidence) => {
                    self.resolve_observed_reuse_target(previous, evidence, prefetched, &promote)?
                }
                None => self.resolve_unknown_reuse_target(Some(previous), &promote)?,
            }
        } else {
            // Older rows have no trusted source provenance. Compare against
            // the exact configured integration ref, never a newly inferred
            // default/upstream fallback, and allow the relation check below
            // to accept only a checkout that already contains it.
            self.resolve_unknown_reuse_target(None, &promote)?
        };

        self.require_base_contains_default(&target.commit, confirmed_default.as_deref())?;
        if target.verify_ref
            && self.repo.resolve_ref(&target.reference).as_deref() != Some(target.commit.as_str())
        {
            return Err(reuse_freshness_refusal(format!(
                "selected base {} changed before checkout validation; retry the task handoff",
                target.reference
            )));
        }

        let (relation, validated_head, checkout_moved_to) = if let Some(sync) =
            integration_sync.as_ref()
        {
            let relation = match sync.outcome {
                AdoptIntegrationSyncOutcome::AlreadyCurrent => {
                    crate::AdoptBaseFreshnessRelation::Current
                }
                AdoptIntegrationSyncOutcome::FastForwarded => {
                    crate::AdoptBaseFreshnessRelation::FastForwarded
                }
            };
            let checkout_moved_to = (sync.outcome == AdoptIntegrationSyncOutcome::FastForwarded)
                .then(|| sync.after_head.clone());
            (relation, sync.after_head.clone(), checkout_moved_to)
        } else {
            let head = checkout.head_commit()?;
            let mut validated_head = head.clone();
            let mut checkout_moved_to = None;
            let branch = checkout.current_branch()?;
            let expected_head_ref = format!("refs/heads/{branch}");
            if branch != session.branch
                || checkout.symbolic_ref("HEAD").as_deref() != Some(expected_head_ref.as_str())
            {
                return Err(reuse_freshness_refusal(
                    "the session checkout is detached or no longer on its recorded branch",
                ));
            }
            // Extra history counts are paid only when refusing a task handoff.
            // A failed count remains unknown and cannot weaken the refusal.
            let stale_refusal = |reason: String| {
                let display_count = |result: Result<u64, crate::GitError>| match result {
                    Ok(count) => count.to_string(),
                    Err(error) => format!("unknown ({error})"),
                };
                let ahead = display_count(checkout.commit_count_between(&target.commit, &head));
                let behind = display_count(checkout.commit_count_between(&head, &target.commit));
                let recorded = previous
                    .as_ref()
                    .map(|observation| observation.target_commit.as_str())
                    .unwrap_or("unknown");
                let age = previous_observation_age
                    .age_seconds
                    .map(|seconds| seconds.to_string())
                    .unwrap_or_else(|| "unknown".into());
                reuse_freshness_refusal(format!(
                    "{reason}; checkout_head={head}; recorded_base_sha={recorded}; \
                     target_ref={}; target_sha={}; ahead={ahead} behind={behind}; \
                     prior_observation_age_seconds={age}; max_age_seconds={}; \
                     next: preserve local changes, inspect and reconcile the session branch \
                     with the selected base, then retry the explicit task handoff",
                    target.reference, target.commit, policy.max_age_seconds,
                ))
            };
            let relation = if head == target.commit {
                crate::AdoptBaseFreshnessRelation::Current
            } else {
                let history_is_complete =
                    checkout
                        .has_complete_unmodified_history()
                        .map_err(|error| {
                            reuse_freshness_refusal(format!(
                                "could not verify the session history against {}: {error}",
                                target.reference
                            ))
                        })?;
                if !history_is_complete {
                    return Err(reuse_freshness_refusal(format!(
                        "the repository has shallow or replaced history, so {} cannot be checked safely; no checkout change was made",
                        target.reference
                    )));
                }
                if checkout
                    .is_ancestor_checked(&target.commit, &head)
                    .map_err(|error| {
                        reuse_freshness_refusal(format!(
                            "could not verify whether {} is contained in the session: {error}",
                            target.reference
                        ))
                    })?
                {
                    crate::AdoptBaseFreshnessRelation::TargetContained
                } else if checkout
                    .is_ancestor_checked(&head, &target.commit)
                    .map_err(|error| {
                        reuse_freshness_refusal(format!(
                            "could not verify whether the session can fast-forward to {}: {error}",
                            target.reference
                        ))
                    })?
                {
                    if !target.evidence.is_some_and(|evidence| {
                        evidence != crate::SessionStartBaseEvidence::ExplicitBase
                    }) {
                        return Err(stale_refusal(format!(
                            "the session checkout is behind the selected base {}; automatic movement is disabled for unknown or explicit base provenance. Recreate the session from that base, or explicitly use --sync-integration if integration is the intended base",
                            target.reference
                        )));
                    }
                    let dirty_paths = checkout.dirty_paths()?;
                    if !dirty_paths.is_empty() {
                        return Err(stale_refusal(format!(
                            "reuse synchronization requires a clean worktree; dirty paths: {dirty_paths:?}; no checkout change was made"
                        )));
                    }
                    let branch = checkout.current_branch()?;
                    let expected_head_ref = format!("refs/heads/{branch}");
                    if branch != session.branch
                        || checkout.symbolic_ref("HEAD").as_deref()
                            != Some(expected_head_ref.as_str())
                    {
                        return Err(reuse_freshness_refusal(
                            "the session checkout is no longer on its recorded branch; no checkout change was made",
                        ));
                    }
                    checkout.fast_forward_checkout_from(&branch, &head, &target.commit)?;
                    let after = checkout.head_commit()?;
                    if after != target.commit
                        || self.repo.resolve_ref(&target.reference).as_deref()
                            != Some(target.commit.as_str())
                    {
                        return Err(reuse_freshness_refusal(format!(
                            "the selected base {} changed during fast-forward; the checkout was moved only to the preflighted commit and must be inspected before reuse",
                            target.reference
                        )));
                    }
                    validated_head = after;
                    checkout_moved_to = Some(target.commit.clone());
                    crate::AdoptBaseFreshnessRelation::FastForwarded
                } else {
                    return Err(stale_refusal(format!(
                        "the session and selected base {} have diverged; no checkout change was made",
                        target.reference
                    )));
                }
            };
            (relation, validated_head, checkout_moved_to)
        };

        if integration_sync.is_none()
            && target.verify_ref
            && self.repo.resolve_ref(&target.reference).as_deref() != Some(target.commit.as_str())
        {
            return Err(reuse_freshness_refusal(format!(
                "selected base {} changed while reuse was being checked; retry the task handoff",
                target.reference
            )));
        }
        if let Some(sync) = integration_sync.as_ref()
            && checkout.head_commit()? != sync.after_head
        {
            return Err(reuse_freshness_refusal(
                "the session HEAD changed during explicit integration synchronization; inspect the checkout before reuse",
            ));
        }
        let final_head = checkout.head_commit()?;
        if validated_head != final_head {
            return Err(reuse_freshness_refusal(
                "the session HEAD changed while reuse freshness was being checked; retry the task boundary",
            ));
        }
        let final_branch = checkout.current_branch()?;
        let final_head_ref = format!("refs/heads/{final_branch}");
        if final_branch != session.branch
            || checkout.symbolic_ref("HEAD").as_deref() != Some(final_head_ref.as_str())
        {
            return Err(reuse_freshness_refusal(
                "the session branch changed while reuse freshness was being checked; retry the task boundary",
            ));
        }

        let checked_at_ms = crate::clock::epoch_ms();
        let observation = crate::broker::SessionBaseObservation {
            schema_version: 1,
            target_ref: target.reference.clone(),
            target_commit: target.commit.clone(),
            evidence: target.evidence,
            observed_at_ms: Some(checked_at_ms),
            promote_branch: target.promote_branch,
            default_ref: target.default_ref.clone(),
        };
        let refresh_diff_base = (relation == crate::AdoptBaseFreshnessRelation::FastForwarded
            || (relation == crate::AdoptBaseFreshnessRelation::Current
                && target.commit == final_head
                && target.evidence.is_some_and(|evidence| {
                    evidence != crate::SessionStartBaseEvidence::ExplicitBase
                })
                && session.diff_base.as_deref() != Some(target.commit.as_str())))
        .then(|| target.commit.clone());
        Ok(ReuseFreshnessDecision {
            report: crate::AdoptBaseFreshness {
                target_ref: target.reference,
                target_commit: target.commit,
                source: target.evidence,
                relation,
                previous_observation_age_seconds: previous_observation_age.age_seconds,
                previous_observation_stale: previous_observation_age.stale,
                max_age_seconds: policy.max_age_seconds,
                observed_at_ms: checked_at_ms,
            },
            observation,
            refresh_diff_base,
            checkout_moved_to,
            integration_sync,
        })
    }

    fn resolve_observed_reuse_target(
        &self,
        observation: &crate::broker::SessionBaseObservation,
        evidence: crate::SessionStartBaseEvidence,
        prefetched: &ReusePrefetchedDefaultBranch,
        promote: &crate::PromoteConfig,
    ) -> Result<ReuseTarget, BrokerOpError> {
        let branch_changed = observation.promote_branch != promote.branch;
        if evidence == crate::SessionStartBaseEvidence::IntegrationTip && branch_changed {
            return Err(reuse_freshness_refusal(format!(
                "the configured integration branch changed from {} to {}; the session base source is unknown under the new policy",
                observation.promote_branch, promote.branch
            )));
        }

        let target = match evidence {
            crate::SessionStartBaseEvidence::ExplicitBase => {
                let commit = self
                    .repo
                    .resolve_ref(&observation.target_commit)
                    .filter(|resolved| resolved == &observation.target_commit)
                    .ok_or_else(|| {
                        reuse_freshness_refusal(
                            "the pinned explicit session base commit is unavailable; it was not replaced by the current ref tip",
                        )
                    })?;
                ReuseTarget {
                    reference: observation.target_ref.clone(),
                    commit,
                    evidence: Some(evidence),
                    default_ref: observation.default_ref.clone(),
                    promote_branch: promote.branch.clone(),
                    verify_ref: false,
                }
            }
            crate::SessionStartBaseEvidence::IntegrationTip => {
                let expected_ref = format!("refs/heads/{}", observation.promote_branch);
                if promote.mode == crate::PromoteMode::VerifyOnly
                    || observation.target_ref != expected_ref
                {
                    return Err(reuse_freshness_refusal(format!(
                        "the stored integration base {} is no longer selected by the current promote policy; choose a new base explicitly",
                        observation.target_ref
                    )));
                }
                let commit = self.repo.resolve_ref(&expected_ref).ok_or_else(|| {
                    reuse_freshness_refusal(format!(
                        "the recorded integration ref {expected_ref} is missing; no fallback base was selected"
                    ))
                })?;
                if let Some(default_ref) = observation.default_ref.as_deref() {
                    // The task boundary already required a fresh default.
                    // Also ensure integration contains that confirmed tip.
                    if prefetched
                        .refresh
                        .as_ref()
                        .is_some_and(|refresh| refresh.fetched)
                    {
                        let current_default =
                            self.require_fresh_default(prefetched, default_ref)?;
                        let history_is_complete = self
                            .repo
                            .has_complete_unmodified_history()
                            .map_err(|error| {
                                reuse_freshness_refusal(format!(
                                    "could not verify integration ancestry: {error}"
                                ))
                            })?;
                        if !history_is_complete
                            || !self
                                .repo
                                .is_ancestor_checked(&current_default, &commit)
                                .map_err(|error| {
                                    reuse_freshness_refusal(format!(
                                        "could not verify that integration contains the fetched default branch: {error}"
                                    ))
                                })?
                        {
                            return Err(reuse_freshness_refusal(format!(
                                "integration {expected_ref} no longer contains the fetched default branch; no fallback base was selected"
                            )));
                        }
                    }
                } else if crate::session_push::tracked_default(&self.repo).is_some() {
                    return Err(reuse_freshness_refusal(
                        "a fetched default branch is now available but the session was started without one; create a new session to select its base",
                    ));
                }
                ReuseTarget {
                    reference: expected_ref,
                    commit,
                    evidence: Some(evidence),
                    default_ref: observation.default_ref.clone(),
                    promote_branch: promote.branch.clone(),
                    verify_ref: true,
                }
            }
            crate::SessionStartBaseEvidence::FetchedDefaultBranch => {
                let default_ref = observation.default_ref.as_deref().ok_or_else(|| {
                    reuse_freshness_refusal(
                        "the fetched-default session base has no recorded remote ref",
                    )
                })?;
                let expected_tracking = format!("refs/remotes/{default_ref}");
                if observation.target_ref != expected_tracking {
                    return Err(reuse_freshness_refusal(
                        "the fetched-default session base ref does not match its recorded remote",
                    ));
                }
                let commit = self.require_fresh_default(prefetched, default_ref)?;
                ReuseTarget {
                    reference: expected_tracking,
                    commit,
                    evidence: Some(evidence),
                    default_ref: Some(default_ref.to_string()),
                    promote_branch: promote.branch.clone(),
                    verify_ref: true,
                }
            }
            crate::SessionStartBaseEvidence::RemoteDefaultBranch => {
                if crate::session_push::tracked_default(&self.repo).is_some() {
                    return Err(reuse_freshness_refusal(
                        "a fetched default branch is now available but the session used a local fallback; create a new session to select its base",
                    ));
                }
                let branch = observation
                    .target_ref
                    .strip_prefix("refs/heads/")
                    .ok_or_else(|| {
                        reuse_freshness_refusal("the recorded local default ref is malformed")
                    })?;
                let expected_remote_head = format!("refs/remotes/origin/{branch}");
                if self
                    .repo
                    .symbolic_ref("refs/remotes/origin/HEAD")
                    .as_deref()
                    != Some(expected_remote_head.as_str())
                    || observation.target_ref != format!("refs/heads/{branch}")
                {
                    return Err(reuse_freshness_refusal(
                        "origin/HEAD no longer selects the recorded local default branch",
                    ));
                }
                let commit = self
                    .repo
                    .resolve_ref(&observation.target_ref)
                    .ok_or_else(|| {
                        reuse_freshness_refusal(format!(
                            "the recorded local default ref {} is missing",
                            observation.target_ref
                        ))
                    })?;
                ReuseTarget {
                    reference: observation.target_ref.clone(),
                    commit,
                    evidence: Some(evidence),
                    default_ref: None,
                    promote_branch: promote.branch.clone(),
                    verify_ref: true,
                }
            }
            crate::SessionStartBaseEvidence::ConventionalMain
            | crate::SessionStartBaseEvidence::ConventionalMaster => {
                if crate::session_push::tracked_default(&self.repo).is_some()
                    || self
                        .repo
                        .symbolic_ref("refs/remotes/origin/HEAD")
                        .and_then(|reference| self.repo.resolve_ref(&reference))
                        .is_some()
                {
                    return Err(reuse_freshness_refusal(
                        "a remote default branch is now available but the session used a conventional local fallback; create a new session to select its base",
                    ));
                }
                let (expected, other) = match evidence {
                    crate::SessionStartBaseEvidence::ConventionalMain => {
                        ("refs/heads/main", "refs/heads/master")
                    }
                    crate::SessionStartBaseEvidence::ConventionalMaster => {
                        ("refs/heads/master", "refs/heads/main")
                    }
                    _ => unreachable!(),
                };
                if observation.target_ref != expected || self.repo.resolve_ref(other).is_some() {
                    return Err(reuse_freshness_refusal(
                        "the conventional local default selection is now ambiguous",
                    ));
                }
                let commit = self.repo.resolve_ref(expected).ok_or_else(|| {
                    reuse_freshness_refusal(format!(
                        "the recorded local default ref {expected} is missing"
                    ))
                })?;
                ReuseTarget {
                    reference: expected.to_string(),
                    commit,
                    evidence: Some(evidence),
                    default_ref: None,
                    promote_branch: promote.branch.clone(),
                    verify_ref: true,
                }
            }
        };
        Ok(target)
    }

    fn resolve_unknown_reuse_target(
        &self,
        previous: Option<&crate::broker::SessionBaseObservation>,
        promote: &crate::PromoteConfig,
    ) -> Result<ReuseTarget, BrokerOpError> {
        if promote.mode == crate::PromoteMode::VerifyOnly {
            return Err(reuse_freshness_refusal(
                "the session base source is unknown and verify-only policy does not select the integration branch; start a new session with an explicit base",
            ));
        }
        let reference = format!("refs/heads/{}", promote.branch);
        let commit = self.repo.resolve_ref(&reference).ok_or_else(|| {
            reuse_freshness_refusal(format!(
                "the session base source is unknown and configured integration ref {reference} is missing; no fallback base was selected"
            ))
        })?;
        Ok(ReuseTarget {
            reference,
            commit,
            evidence: None,
            default_ref: previous.and_then(|observation| observation.default_ref.clone()),
            promote_branch: promote.branch.clone(),
            verify_ref: true,
        })
    }

    fn require_fresh_default(
        &self,
        prefetched: &ReusePrefetchedDefaultBranch,
        expected_display_ref: &str,
    ) -> Result<String, BrokerOpError> {
        let refresh = prefetched.refresh.as_ref().ok_or_else(|| {
            reuse_freshness_refusal(format!(
                "could not confirm {expected_display_ref} during this handoff; retry after the default-branch refresh succeeds"
            ))
        })?;
        if !refresh.fetched || refresh.display_ref != expected_display_ref {
            let detail = prefetched
                .note
                .as_deref()
                .map(|note| format!(" ({note})"))
                .unwrap_or_default();
            return Err(reuse_freshness_refusal(format!(
                "the selected default branch {expected_display_ref} was not freshly confirmed{detail}; retry the task handoff after fetch succeeds"
            )));
        }
        let current = crate::session_push::tracked_default(&self.repo).ok_or_else(|| {
            reuse_freshness_refusal(format!(
                "the recorded default branch {expected_display_ref} no longer resolves"
            ))
        })?;
        if current.tracking_ref != refresh.tracking_ref
            || format!("{}/{}", current.remote, current.branch) != expected_display_ref
        {
            return Err(reuse_freshness_refusal(
                "the repository's selected default branch changed during this handoff; retry the task boundary",
            ));
        }
        let observed = refresh.commit.as_deref().ok_or_else(|| {
            reuse_freshness_refusal(format!(
                "the fetched default branch {expected_display_ref} has no readable commit"
            ))
        })?;
        if current.commit != observed
            || self.repo.resolve_ref(&refresh.tracking_ref).as_deref() != Some(observed)
        {
            return Err(reuse_freshness_refusal(
                "the fetched default ref changed after measurement; retry the task boundary",
            ));
        }
        Ok(observed.to_string())
    }

    fn require_base_contains_default(
        &self,
        base: &str,
        confirmed_default: Option<&str>,
    ) -> Result<(), BrokerOpError> {
        let Some(default) = confirmed_default else {
            return Ok(());
        };
        if !self
            .repo
            .has_complete_unmodified_history()
            .map_err(|error| {
                reuse_freshness_refusal(format!("could not verify selected base history: {error}"))
            })?
            || !self
                .repo
                .is_ancestor_checked(default, base)
                .map_err(|error| {
                    reuse_freshness_refusal(format!(
                        "could not verify selected base ancestry: {error}"
                    ))
                })?
        {
            return Err(reuse_freshness_refusal(
                "the selected base does not provably contain the freshly fetched default branch; reconcile integration or create a new session from the default branch. No checkout movement was authorized",
            ));
        }
        Ok(())
    }

    fn default_branch_after_reuse(
        &self,
        checkout: &GitRepo,
        session: &Session,
        prefetched: &ReusePrefetchedDefaultBranch,
        freshness: &ReuseFreshnessDecision,
    ) -> (Option<crate::DefaultBranchDrift>, Option<String>) {
        if freshness.refresh_diff_base.is_none() {
            return (prefetched.drift.clone(), prefetched.note.clone());
        }
        let Some(refresh) = prefetched.refresh.as_ref() else {
            return (prefetched.drift.clone(), prefetched.note.clone());
        };
        let Some(commit) = refresh.commit.as_deref() else {
            return (prefetched.drift.clone(), prefetched.note.clone());
        };
        let Some((remote, _)) = refresh.display_ref.split_once('/') else {
            return (prefetched.drift.clone(), prefetched.note.clone());
        };
        let head = match checkout.head_commit() {
            Ok(head) => head,
            Err(error) => {
                return (
                    prefetched.drift.clone(),
                    Some(format!(
                        "base reuse succeeded, but the refreshed drift report could not read HEAD: {error}"
                    )),
                );
            }
        };
        let catch_up = crate::main_drift::CatchUp::for_branch(checkout, remote, &session.branch);
        match crate::main_drift::measure(checkout, &refresh.display_ref, commit, &head, catch_up) {
            Ok(drift) => (Some(drift), prefetched.note.clone()),
            Err(error) => {
                let note = format!(
                    "base reuse succeeded, but the refreshed default-branch comparison failed: {error}"
                );
                (prefetched.drift.clone(), Some(note))
            }
        }
    }

    pub(super) fn synchronize_reuse_checkout(
        &mut self,
        checkout: &GitRepo,
        integration_branch: &str,
        integration_head: &str,
        session_branch: &str,
    ) -> Result<AdoptIntegrationSync, BrokerOpError> {
        let integration_ref = format!("refs/heads/{integration_branch}");
        if self.repo.resolve_ref(&integration_ref).as_deref() != Some(integration_head) {
            return Err(reuse_freshness_refusal(format!(
                "configured integration ref {integration_ref} changed during preflight; retry the task handoff"
            )));
        }
        let dirty_paths = checkout.dirty_paths()?;
        if !dirty_paths.is_empty() {
            return Err(BrokerOpError::ReuseSyncDirty { paths: dirty_paths });
        }

        let before_head = checkout.head_commit()?;
        let branch = checkout.current_branch()?;
        let expected_head_ref = format!("refs/heads/{branch}");
        if branch != session_branch
            || checkout.symbolic_ref("HEAD").as_deref() != Some(expected_head_ref.as_str())
        {
            return Err(reuse_freshness_refusal(
                "the session checkout is detached or no longer on its recorded branch",
            ));
        }
        if before_head != integration_head
            && !checkout
                .has_complete_unmodified_history()
                .map_err(|error| {
                    reuse_freshness_refusal(format!(
                        "could not verify the session history before integration synchronization: {error}"
                    ))
                })?
        {
            return Err(reuse_freshness_refusal(
                "the repository has shallow or replaced history, so integration synchronization cannot be verified",
            ));
        }
        let outcome = if before_head == integration_head {
            AdoptIntegrationSyncOutcome::AlreadyCurrent
        } else {
            if !checkout
                .is_ancestor_checked(&before_head, integration_head)
                .map_err(|error| {
                    reuse_freshness_refusal(format!(
                        "could not verify the requested integration fast-forward: {error}"
                    ))
                })?
            {
                let ahead = checkout.commit_count_between(integration_head, &before_head)?;
                let behind = checkout.commit_count_between(&before_head, integration_head)?;
                let relation = match (ahead, behind) {
                    (_, 0) => AdoptIntegrationRelation::Ahead,
                    _ => AdoptIntegrationRelation::Diverged,
                };
                return Err(BrokerOpError::ReuseSyncNotFastForward {
                    session_head: before_head,
                    integration_head: integration_head.to_owned(),
                    relation: relation.as_str(),
                });
            }
            checkout.fast_forward_checkout_from(&branch, &before_head, integration_head)?;
            AdoptIntegrationSyncOutcome::FastForwarded
        };

        let after_head = checkout.head_commit()?;
        if after_head != integration_head {
            return Err(BrokerOpError::ReuseSyncVerification {
                expected: integration_head.to_string(),
                actual: after_head,
            });
        }
        if self.repo.resolve_ref(&integration_ref).as_deref() != Some(integration_head) {
            return Err(reuse_freshness_refusal(format!(
                "configured integration ref {integration_ref} moved during synchronization; inspect the checkout before reuse"
            )));
        }
        Ok(AdoptIntegrationSync {
            outcome,
            integration_branch: integration_branch.to_string(),
            integration_head: integration_head.to_string(),
            before_head,
            after_head,
        })
    }

    /// `may_create_integration` keeps reuse behaviour unchanged while letting a
    /// plain adopt report drift without bringing the integration branch into
    /// existence as a side effect of describing state (issue #144).
    pub(super) fn adopt_integration_drift(
        &mut self,
        checkout: &GitRepo,
        session_id: i64,
        may_create_integration: bool,
    ) -> Result<AdoptIntegrationDrift, BrokerOpError> {
        let session_head = checkout.head_commit()?;
        let (integration_branch, integration_head) = if may_create_integration {
            self.integration_head()?
        } else {
            self.integration_head_snapshot()?
        };
        if session_head == integration_head {
            return Ok(AdoptIntegrationDrift {
                session_head,
                integration_branch,
                integration_head,
                relation: AdoptIntegrationRelation::Current,
                ahead_commits: 0,
                behind_commits: 0,
                overlapping_changed_paths: Vec::new(),
                warning: None,
                safe_next_action: format!(
                    "continue with session {session_id} on the current integration baseline"
                ),
            });
        }
        let ahead_commits = checkout.commit_count_between(&integration_head, &session_head)?;
        let behind_commits = checkout.commit_count_between(&session_head, &integration_head)?;
        let relation = match (ahead_commits, behind_commits) {
            (0, 0) => AdoptIntegrationRelation::Current,
            (0, _) => AdoptIntegrationRelation::Behind,
            (_, 0) => AdoptIntegrationRelation::Ahead,
            _ => AdoptIntegrationRelation::Diverged,
        };

        let overlapping_changed_paths = checkout
            .merge_base(&session_head, &integration_head)
            .ok()
            .map(|base| -> Result<Vec<String>, BrokerOpError> {
                let session_paths = checkout.changed_files(&base)?;
                let integration_paths = checkout.changed_between(&base, &integration_head)?;
                let mut overlaps = session_paths
                    .into_iter()
                    .filter(|session_path| {
                        integration_paths.iter().any(|integration_path| {
                            crate::leases::paths_overlap(session_path, integration_path)
                        })
                    })
                    .collect::<Vec<_>>();
                overlaps.sort();
                overlaps.dedup();
                Ok(overlaps)
            })
            .transpose()?
            .unwrap_or_default();

        // Only the Ahead guidance consumes submission provenance. Behind and
        // diverged checkouts need a drift warning, not a patch-id scan of the
        // repository's history while adoption holds the lifecycle claim.
        let submission_plan = if matches!(relation, AdoptIntegrationRelation::Ahead) {
            self.store.session(session_id).ok().and_then(|session| {
                self.build_submission_plan(&session, &session_head, &integration_head)
                    .ok()
            })
        } else {
            None
        };
        let pending_owned_commits = submission_plan
            .as_ref()
            .map(|plan| plan.pending_owned_commit_ids().len());
        let submission_plan_safe = submission_plan.as_ref().is_some_and(|plan| plan.safe);

        let (warning, safe_next_action) = match relation {
            AdoptIntegrationRelation::Current => (
                None,
                format!("continue with session {session_id} on the current integration baseline"),
            ),
            AdoptIntegrationRelation::Behind => (
                Some(format!(
                    "session HEAD is {behind_commits} commit(s) behind {integration_branch}; inspect drift before editing"
                )),
                "aethyme broker advanced integration status".into(),
            ),
            AdoptIntegrationRelation::Ahead
                if submission_plan_safe && pending_owned_commits.is_some_and(|count| count > 0) =>
            {
                (
                    Some(format!(
                        "session HEAD is {ahead_commits} commit(s) ahead of {integration_branch}; {pending} pending session-owned commit(s) are safe to submit before starting a follow-up",
                        pending = pending_owned_commits.unwrap_or_default()
                    )),
                    format!("aethyme broker submit --session {session_id}"),
                )
            }
            AdoptIntegrationRelation::Ahead => (
                Some(format!(
                    "session HEAD is {ahead_commits} commit(s) ahead of {integration_branch}, but none are session-owned under the recorded baseline, so submit will not replay them; they predate this adoption. To submit them, re-adopt from a base that precedes them"
                )),
                "aethyme broker advanced integration status".into(),
            ),
            AdoptIntegrationRelation::Diverged => (
                Some(format!(
                    "session HEAD and {integration_branch} have diverged ({ahead_commits} ahead, {behind_commits} behind); reconcile before editing"
                )),
                "aethyme broker advanced integration status".into(),
            ),
        };

        Ok(AdoptIntegrationDrift {
            session_head,
            integration_branch,
            integration_head,
            relation,
            ahead_commits,
            behind_commits,
            overlapping_changed_paths,
            warning,
            safe_next_action,
        })
    }

    /// Close broker state without removing the worktree. Policy-eligible,
    /// ignored build artifacts may be reclaimed while the checkout is retained.
    pub fn close(&mut self, session_id: i64) -> Result<(), BrokerOpError> {
        let session = self.store.session(session_id)?;
        if let Some((head, unpushed_commits)) = self.unpushed_close_check(&session) {
            return Err(BrokerOpError::UnpushedSessionWork {
                session_id,
                branch: session.branch,
                head,
                unpushed_commits,
            });
        }
        self.close_unchecked(session_id)
    }

    /// [`Broker::close`] for a session whose unpushed commits the caller has
    /// decided to leave behind. The decision and its reason are recorded, so
    /// the work is abandoned on the record rather than silently.
    pub fn close_abandoning_unpushed(
        &mut self,
        session_id: i64,
        reason: &str,
    ) -> Result<(), BrokerOpError> {
        let session = self.store.session(session_id)?;
        if let Some((head, unpushed_commits)) = self.unpushed_close_check(&session) {
            self.record_abandoned_unpushed(&session, &head, unpushed_commits, reason)?;
        }
        self.close_unchecked(session_id)
    }

    /// Commits only this session's worktree holds, when the repository opted
    /// into the push lane and has a remote to push to; `None` otherwise.
    pub(crate) fn unpushed_close_check(&self, session: &Session) -> Option<(String, u32)> {
        if !crate::session_push::session_push_enabled(&self.repo)
            || self.repo.remotes().unwrap_or_default().is_empty()
        {
            return None;
        }
        let upstream = self
            .publication_baseline()
            .ok()
            .map(|(reference, _)| reference)
            .filter(|reference| reference.starts_with("refs/remotes/"));
        let integration_head = self.integration_head_snapshot().ok().map(|(_, head)| head);
        let (head, work) =
            session_off_remote_work(session, upstream.as_deref(), integration_head.as_deref())
                .ok()??;
        (work.commits > 0).then_some((head, work.commits))
    }

    pub(super) fn record_abandoned_unpushed(
        &mut self,
        session: &Session,
        head: &str,
        unpushed_commits: u32,
        reason: &str,
    ) -> Result<(), BrokerOpError> {
        let payload = crate::events::session_abandoned_unpushed_payload(
            session.id,
            &session.branch,
            head,
            unpushed_commits,
            reason,
        );
        self.store.append_event(
            crate::events::BROKER_SESSION_ABANDONED_UNPUSHED,
            Some(session.id),
            Some(&payload),
        )?;
        Ok(())
    }

    pub(super) fn close_unchecked(&mut self, session_id: i64) -> Result<(), BrokerOpError> {
        self.store
            .set_session_status(session_id, SessionStatus::Closed, None)?;
        // Closing must not fail because a best-effort artifact sweep did;
        // report the failure instead of discarding it.
        crate::warn_unrecorded(
            "reclaim a closed session's build artifacts",
            self.reclaim_closed_session_artifacts(session_id),
        );
        Ok(())
    }

    /// Ordinary starts are intentionally anchored to integration. A caller
    /// that explicitly identifies a pull request must use the routed review
    /// adapter, which provisions and verifies that pull request's exact head.
    pub(crate) fn reject_integration_based_review_task(
        pull_request: Option<i64>,
    ) -> Result<(), BrokerOpError> {
        if let Some(pull_request) = pull_request {
            return Err(BrokerOpError::ReviewRequiresPullRequestHead { pull_request });
        }
        Ok(())
    }

    // ── start-agent (spawn convenience) ───────────────────────────────

    /// Create a broker-owned worktree + branch for `task` without
    /// spawning a process. This is the preferred entrypoint for agents
    /// already running in an existing shell: the caller can `cd` into the
    /// returned path and continue with an isolated index and checkout.
    /// The caller here IS the agent that will work the worktree -- unlike
    /// [`Broker::start_agent`], no separate process is involved -- so
    /// `agent_identity` is its own identity for promote-commit credit.
    pub fn start_worktree(
        &mut self,
        task: &str,
        agent_identity: Option<&str>,
    ) -> Result<Session, BrokerOpError> {
        Ok(self
            .start_worktree_with_planned_paths(task, &[], agent_identity)?
            .session)
    }

    pub fn start_worktree_with_planned_paths(
        &mut self,
        task: &str,
        paths: &[String],
        agent_identity: Option<&str>,
    ) -> Result<StartReport, BrokerOpError> {
        self.start_worktree_with_planned_paths_and_context(
            task,
            paths,
            agent_identity,
            SessionContext::default(),
            None,
        )
    }

    pub fn start_worktree_with_planned_paths_and_context(
        &mut self,
        task: &str,
        paths: &[String],
        agent_identity: Option<&str>,
        context: SessionContext,
        explicit_base: Option<&str>,
    ) -> Result<StartReport, BrokerOpError> {
        self.start_worktree_with_planned_paths_and_context_timed(
            task,
            paths,
            agent_identity,
            context,
            explicit_base,
        )
        .map(|timed| timed.report)
    }

    pub(crate) fn start_worktree_with_planned_paths_and_context_timed(
        &mut self,
        task: &str,
        paths: &[String],
        agent_identity: Option<&str>,
        context: SessionContext,
        explicit_base: Option<&str>,
    ) -> Result<TimedStartReport<StartReport>, BrokerOpError> {
        let context = self.session_context(context);
        let planned_paths = normalize_planned_paths(paths)?;
        let CreatedSessionWorktree {
            mut phase_timings_ms,
            slug: _,
            branch,
            start_base,
            worktree,
            worktree_placement,
            selected_promote_branch,
            _lifecycle_claim,
        } = self.create_session_worktree(task, explicit_base, &planned_paths)?;
        let base = start_base.commit.clone();
        let base_observation = crate::broker::SessionBaseObservation::from_start_base(
            &start_base,
            selected_promote_branch,
        );
        let metadata_started = std::time::Instant::now();
        let worktree_instance_witness = capture_spawned_worktree_instance_witness(&worktree);
        let repository_contract = self.capture_repository_contract(worktree.root(), false)?;
        let new_session = NewSession {
            worktree_path: worktree.root().to_string_lossy().into_owned(),
            branch: branch.clone(),
            origin: SessionOrigin::Spawned,
            task: Some(task.to_string()),
            adoption_base: Some(base.clone()),
            adopted_head: Some(base.clone()),
            diff_base: Some(base.clone()),
            repository_contract: Some(repository_contract),
            pid: None,
            command: None,
            log_path: None,
            agent_identity: agent_identity.map(str::to_string),
        };
        let session = match self
            .store
            .register_spawned_session_with_context_and_leases_and_base_observation(
                LeaseRefusalPolicy::verify_only_at(&self.main_root),
                &new_session,
                &context,
                &planned_paths,
                worktree_instance_witness.as_ref(),
                Some(&base_observation),
            ) {
            Ok(session) => session,
            Err(error) => {
                let worktree_path = worktree.root().to_path_buf();
                let removal = self.repo.worktree_remove_registered(&worktree_path, true);
                if let Err(removal_error) = removal {
                    return Err(BrokerOpError::Git(crate::GitError::Git {
                        args: format!("remove {}", worktree_path.display()),
                        stderr: format!(
                            "session registration failed: {error}; rollback failed: {removal_error}; retained worktree and branch {branch} for inspection"
                        ),
                    }));
                }
                if !path_is_explicitly_absent(&worktree_path) {
                    return Err(BrokerOpError::Git(crate::GitError::Git {
                        args: format!("remove {}", worktree_path.display()),
                        stderr: format!(
                            "session registration failed: {error}; Git reported removal but the path is not positively absent; retained worktree and branch {branch} for inspection"
                        ),
                    }));
                }
                if let Err(branch_error) = self.repo.delete_branch_ref_checked(&branch, &base) {
                    return Err(BrokerOpError::Git(crate::GitError::Git {
                        args: format!("delete refs/heads/{branch}"),
                        stderr: format!(
                            "session registration failed: {error}; worktree was removed, but branch rollback could not be confirmed: {branch_error}"
                        ),
                    }));
                }
                return Err(error.into());
            }
        };
        self.store.set_session_foreign_files(session.id, &[])?;
        let planned_explicit_leases = self.planned_explicit_leases(session.id, &planned_paths)?;
        phase_timings_ms.insert(
            "session_registration".into(),
            u64::try_from(metadata_started.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
        let preparation = timed_start_phase(&mut phase_timings_ms, "preparation_status", || {
            self.preparation_status(session.id)
        })?;
        Ok(TimedStartReport {
            phase_timings_ms,
            report: StartReport {
                session,
                start_base,
                worktree_placement,
                planned_explicit_leases,
                preparation,
            },
        })
    }

    /// Create a worktree + branch for `task` and spawn `command` in it via
    /// `sh -c` with stdout/stderr teed to a log file. Returns the session;
    /// the child runs detached (the broker never owns the process beyond
    /// recording its PID).
    pub fn start_agent(
        &mut self,
        task: &str,
        command: &str,
        agent_identity: Option<&str>,
    ) -> Result<Session, BrokerOpError> {
        Ok(self
            .start_agent_report(task, command, agent_identity)?
            .session)
    }

    pub fn start_agent_report(
        &mut self,
        task: &str,
        command: &str,
        agent_identity: Option<&str>,
    ) -> Result<StartAgentReport, BrokerOpError> {
        self.start_agent_report_with_context(
            task,
            command,
            agent_identity,
            SessionContext::default(),
            None,
        )
    }

    pub fn start_agent_report_with_context(
        &mut self,
        task: &str,
        command: &str,
        agent_identity: Option<&str>,
        context: SessionContext,
        explicit_base: Option<&str>,
    ) -> Result<StartAgentReport, BrokerOpError> {
        self.start_agent_report_with_context_timed(
            task,
            command,
            agent_identity,
            context,
            explicit_base,
        )
        .map(|timed| timed.report)
    }

    pub(crate) fn start_agent_report_with_context_timed(
        &mut self,
        task: &str,
        command: &str,
        agent_identity: Option<&str>,
        context: SessionContext,
        explicit_base: Option<&str>,
    ) -> Result<TimedStartReport<StartAgentReport>, BrokerOpError> {
        crate::agent_supervisor::ensure_supported().map_err(|source| BrokerOpError::Spawn {
            command: command.to_owned(),
            source,
        })?;
        crate::agent_supervisor::resolve_executable(self.agent_supervisor_executable.as_deref())
            .map_err(|source| BrokerOpError::Spawn {
                command: command.to_owned(),
                source,
            })?;
        let context = self.session_context(context);
        let CreatedSessionWorktree {
            mut phase_timings_ms,
            slug,
            branch,
            start_base,
            worktree,
            worktree_placement,
            selected_promote_branch,
            _lifecycle_claim,
        } = self.create_session_worktree(task, explicit_base, &[])?;
        let agent_setup_started = std::time::Instant::now();
        let base = start_base.commit.clone();
        let base_observation = crate::broker::SessionBaseObservation::from_start_base(
            &start_base,
            selected_promote_branch,
        );
        let worktree_path = worktree.root().to_path_buf();
        let worktree_instance_witness = capture_spawned_worktree_instance_witness(&worktree);
        let repository_contract = match self.capture_repository_contract(&worktree_path, false) {
            Ok(contract) => contract,
            Err(error) => {
                return Err(start_agent_setup_failure(
                    &self.repo,
                    &worktree_path,
                    &branch,
                    &base,
                    None,
                    error,
                ));
            }
        };

        let log_dir = self.main_root.join(".aethyme/logs");
        let (created_log, log_file) = match create_agent_log(&log_dir, &slug) {
            Ok(log) => log,
            Err(error) => {
                return Err(start_agent_setup_failure(
                    &self.repo,
                    &worktree_path,
                    &branch,
                    &base,
                    None,
                    error,
                ));
            }
        };
        let log_path = created_log.path.clone();
        let log_clone = match log_file.try_clone() {
            Ok(log) => log,
            Err(source) => {
                let error = BrokerOpError::Store(BrokerError::Io {
                    path: log_path.clone(),
                    source,
                });
                return Err(start_agent_setup_failure(
                    &self.repo,
                    &worktree_path,
                    &branch,
                    &base,
                    Some(&created_log),
                    error,
                ));
            }
        };

        let mut agent =
            match spawn_blocked_agent(command, &worktree_path, log_file, log_clone, Some(self)) {
                Ok(agent) => agent,
                Err(error) => {
                    return Err(start_agent_setup_failure(
                        &self.repo,
                        &worktree_path,
                        &branch,
                        &base,
                        Some(&created_log),
                        error,
                    ));
                }
            };
        let pid = agent.pid();
        let worktree_path_string = worktree_path.to_string_lossy().into_owned();
        let log_path_string = log_path.to_string_lossy().into_owned();
        let registration = self
            .store
            .register_spawned_session_with_context_and_leases_and_base_observation(
                LeaseRefusalPolicy::verify_only_at(&self.main_root),
                &NewSession {
                    worktree_path: worktree_path_string.clone(),
                    branch: branch.clone(),
                    origin: SessionOrigin::Spawned,
                    task: Some(task.to_string()),
                    adoption_base: Some(base.clone()),
                    adopted_head: Some(base.clone()),
                    diff_base: Some(base.clone()),
                    repository_contract: Some(repository_contract),
                    pid: Some(pid as i64),
                    command: Some(command.to_string()),
                    log_path: Some(log_path_string.clone()),
                    agent_identity: agent_identity.map(str::to_string),
                },
                &context,
                &[],
                worktree_instance_witness.as_ref(),
                Some(&base_observation),
            );
        let session = match registration {
            Ok(session) => session,
            Err(registration_error) => {
                let primary = BrokerOpError::Store(registration_error);
                match self.store.session_for_worktree(&worktree_path_string) {
                    Ok(Some(session))
                        if start_agent_registration_matches(
                            &session,
                            &worktree_path_string,
                            &branch,
                            &base,
                            pid,
                            command,
                            &log_path_string,
                        ) =>
                    {
                        // Registration commits before its confirmation read.
                        // This exact row is sufficient to continue if that
                        // post-commit read was the failing step.
                        session
                    }
                    Ok(None) => {
                        if let Err(reason) = agent.abort() {
                            return Err(BrokerOpError::StartAgentRollback {
                                primary: Box::new(primary),
                                recovery: vec![format!(
                                    "gated agent pid {pid} did not stop/reap ({reason}); retained worktree {worktree_path_string} and log {log_path_string}"
                                )],
                            });
                        }
                        return Err(start_agent_setup_failure(
                            &self.repo,
                            &worktree_path,
                            &branch,
                            &base,
                            Some(&created_log),
                            primary,
                        ));
                    }
                    Ok(Some(_)) => {
                        let cleanup = agent
                            .abort()
                            .map(|()| "; gated child was stopped and reaped".to_string())
                            .unwrap_or_else(|reason| {
                                format!("; gated child stop/reap is incomplete: {reason}")
                            });
                        return Err(BrokerOpError::StartAgentRegistrationUncertain {
                            registration_error: primary.to_string(),
                            pid,
                            worktree_path: worktree_path_string,
                            log_path: log_path_string,
                            cleanup,
                        });
                    }
                    Err(probe_error) => {
                        let cleanup = agent
                            .abort()
                            .map(|()| "; gated child was stopped and reaped".to_string())
                            .unwrap_or_else(|reason| {
                                format!("; gated child stop/reap is incomplete: {reason}")
                            });
                        return Err(BrokerOpError::StartAgentRegistrationUncertain {
                            registration_error: format!(
                                "{primary}; exact-row probe failed: {probe_error}"
                            ),
                            pid,
                            worktree_path: worktree_path_string,
                            log_path: log_path_string,
                            cleanup,
                        });
                    }
                }
            }
        };

        if let Err(source) = agent.release() {
            let cleanup = match agent.abort() {
                Ok(()) => match self.store.set_session_status(
                    session.id,
                    SessionStatus::Exited,
                    Some(125),
                ) {
                    Ok(()) => "; gated child was stopped/reaped and session marked exited".into(),
                    Err(error) => format!(
                        "; gated child was stopped/reaped, but marking the registered session exited failed: {error}"
                    ),
                },
                Err(reason) => format!("; gated child stop/reap is incomplete: {reason}"),
            };
            return Err(BrokerOpError::StartAgentRelease {
                session_id: session.id,
                pid,
                worktree_path: worktree_path_string.into_boxed_str(),
                log_path: log_path_string.into_boxed_str(),
                reason: source.to_string(),
                cleanup,
            });
        }

        phase_timings_ms.insert(
            "agent_setup".into(),
            u64::try_from(agent_setup_started.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
        Ok(TimedStartReport {
            phase_timings_ms,
            report: StartAgentReport {
                session,
                start_base,
                worktree_placement,
            },
        })
    }

    fn create_session_worktree(
        &mut self,
        task: &str,
        explicit_base: Option<&str>,
        planned_paths: &[String],
    ) -> Result<CreatedSessionWorktree, BrokerOpError> {
        let mut phase_timings_ms = std::collections::BTreeMap::new();
        let refresh = timed_start_phase(&mut phase_timings_ms, "upstream_fetch", || {
            self.refresh_default_branch_before_start()
        });
        refresh.require_fresh_implicit_base(explicit_base)?;
        // Allocation needs a fresh base, not integration maintenance. In
        // verify-only mode selection below already uses the fetched default.
        // Reconciling here reads historical promotions and can wait on an
        // unrelated writer; submit and sync refresh integration themselves.
        let selected_promote = crate::merge::PromoteConfig::load(&self.main_root);
        let mut start_base = timed_start_phase(&mut phase_timings_ms, "base_selection", || {
            self.select_session_start_base_with_promote(explicit_base, &selected_promote)
        })?;
        refresh.record(&mut start_base);
        // Refresh may wait on another process or a remote. Acquire the
        // repository lifecycle claim only after that work. Root preparation
        // writes the marker and build defaults, so it belongs under the same
        // claim as storage's empty-root removal; otherwise a confirmed sweep
        // could delete a root prepared before this final claim.
        let lifecycle_claim =
            timed_start_phase(&mut phase_timings_ms, "lifecycle_lock_wait", || {
                self.claim_worktree_lifecycle()
            })?;
        timed_start_phase(&mut phase_timings_ms, "checkout_count", || {
            self.preflight_worktree_checkout_count()
        })?;
        let placement = timed_start_phase(&mut phase_timings_ms, "worktree_root", || {
            self.prepare_broker_worktree_root()
        })?;
        // Choose the unique branch and recheck the destination immediately
        // before the checkout is allocated. The returned guard stays held
        // until the caller registers the session or rolls the checkout back.
        self.ensure_planned_paths_available(planned_paths, None)?;
        let slug = self.next_worktree_slug(task, &placement.root);
        let worktree_path = placement.root.join(&slug);
        self.refuse_nested_worktree_path(&worktree_path)?;
        let branch = format!("agent/{slug}");
        timed_start_phase(&mut phase_timings_ms, "storage_admission", || {
            self.preflight_session_checkout_storage(&placement)
        })?;
        let worktree = match timed_start_phase(&mut phase_timings_ms, "git_worktree_add", || {
            self.repo
                .worktree_add(&worktree_path, &branch, &start_base.commit)
        }) {
            Ok(worktree) => worktree,
            Err(error) => {
                let primary = BrokerOpError::Git(error);
                let recovery =
                    inspect_partial_worktree_creation(&self.repo, &worktree_path, &branch);
                if recovery.is_empty() {
                    return Err(primary);
                }
                return Err(BrokerOpError::StartAgentRollback {
                    primary: Box::new(primary),
                    recovery,
                });
            }
        };
        if placement.source == WorktreeRootSource::RepositoryConfig {
            // A failed lock must not fail the start, but it leaves the
            // registration unprotected while the drive is unplugged, so say so.
            crate::warn_unrecorded(
                "lock the worktree on the configured root against pruning",
                self.repo.worktree_lock(&worktree_path),
            );
        }
        Ok(CreatedSessionWorktree {
            phase_timings_ms,
            slug,
            branch,
            start_base,
            worktree,
            worktree_placement: placement,
            selected_promote_branch: selected_promote.branch,
            _lifecycle_claim: lifecycle_claim,
        })
    }

    pub fn worktree_root_plan(&self) -> Result<WorktreeRootPlan, BrokerOpError> {
        self.storage_context.ensure_valid()?;
        let git_common_dir = self.main_git_common_dir()?;
        let mut context = self.storage_context.clone();
        if let Some(root) = &self.worktree_root_override {
            context.worktree_root_override = Some(root.clone());
            context.worktree_root_environment_override = None;
        }
        let mut plan =
            plan_worktree_root_with_context(&self.main_root, Some(&git_common_dir), &context);
        let binding = self.store.repository_worktree_binding()?;
        if binding.state == crate::store::repository_binding::RepositoryWorktreeBindingState::Bound
            && let Some(root) = binding.worktree_root
        {
            plan.preferred_root = Some(root);
            plan.preferred_source = binding.root_source;
            plan.preferred_outside_repository = plan
                .preferred_root
                .as_deref()
                .is_some_and(|root| !self.path_is_inside_repository(root));
            plan.host_state_fallback_root = None;
            plan.preferred_unavailable_reason = None;
            plan.root_container = if binding.root_layout
                == Some(
                    crate::store::repository_binding::RepositoryWorktreeRootLayout::KeyedContainer,
                ) {
                plan.preferred_root
                    .as_ref()
                    .and_then(|root| root.parent().map(Path::to_path_buf))
            } else {
                None
            };
        }
        Ok(plan)
    }

    /// Verify the durable root binding before any startup or root-dependent
    /// operation writes. `None` means the database is readable but still
    /// needs an explicit fresh reservation or legacy adoption.
    pub(crate) fn validate_bound_repository_worktree_root(
        &self,
    ) -> Result<Option<PathBuf>, BrokerOpError> {
        use crate::store::repository_binding::RepositoryWorktreeBindingState;

        let binding = self.store.repository_worktree_binding()?;
        if binding.state != RepositoryWorktreeBindingState::Bound {
            return Ok(None);
        }
        let refusal = |reason: String| BrokerOpError::WorktreeRootUnavailable {
            path: binding
                .worktree_root
                .clone()
                .unwrap_or_else(|| self.main_root.join(".aethyme/worktrees")),
            reason,
        };
        let repository_root = binding
            .repository_root
            .as_deref()
            .ok_or_else(|| refusal("bound record has no repository root".into()))?;
        let git_common_dir = binding
            .git_common_dir
            .as_deref()
            .ok_or_else(|| refusal("bound record has no Git common directory".into()))?;
        let repository_key = binding
            .repository_key
            .as_deref()
            .ok_or_else(|| refusal("bound record has no repository key".into()))?;
        let database_path = binding
            .broker_db_path
            .as_deref()
            .ok_or_else(|| refusal("bound record has no broker database path".into()))?;
        let root = binding
            .worktree_root
            .as_deref()
            .ok_or_else(|| refusal("bound record has no worktree root".into()))?;
        let stored_parent = binding
            .parent_identity_json
            .as_deref()
            .ok_or_else(|| refusal("bound record has no parent identity".into()))?;
        let stored_root = binding
            .worktree_identity_json
            .as_deref()
            .ok_or_else(|| refusal("bound record has no worktree identity".into()))?;
        let current_repository_root = self
            .main_root
            .canonicalize()
            .map_err(|error| refusal(format!("cannot verify repository root: {error}")))?;
        let current_git_common_dir = self
            .main_git_common_dir()?
            .canonicalize()
            .map_err(|error| refusal(format!("cannot verify Git common directory: {error}")))?;
        let current_repository_key = crate::host_state::repository_key(
            &current_repository_root,
            Some(&current_git_common_dir),
        );
        let current_database_path = self
            .store
            .db_path()
            .canonicalize()
            .map_err(|error| refusal(format!("cannot verify broker database path: {error}")))?;
        if repository_root != current_repository_root
            || git_common_dir != current_git_common_dir
            || repository_key != current_repository_key
            || database_path != current_database_path
        {
            return Err(refusal(
                "stored repository or database identity differs from this checkout".into(),
            ));
        }
        let root_metadata = std::fs::symlink_metadata(root)
            .map_err(|error| refusal(format!("cannot inspect bound worktree root: {error}")))?;
        if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
            return Err(refusal(
                "bound worktree root is not a real directory".into(),
            ));
        }
        let canonical_root = root.canonicalize().map_err(|error| {
            refusal(format!("cannot canonicalize bound worktree root: {error}"))
        })?;
        if canonical_root != root {
            return Err(refusal(
                "bound worktree root no longer has its recorded canonical path".into(),
            ));
        }
        verify_private_worktree_root_permissions(root).map_err(|reason| {
            refusal(format!(
                "cannot verify bound worktree-root permissions: {reason}"
            ))
        })?;
        let parent = root
            .parent()
            .ok_or_else(|| refusal("bound worktree root has no parent".into()))?;
        let observed_parent = bound_directory_identity(parent)
            .map_err(|reason| refusal(format!("cannot verify bound root parent: {reason}")))?;
        let observed_root = bound_directory_identity(root)
            .map_err(|reason| refusal(format!("cannot verify bound root identity: {reason}")))?;
        let expected_parent: BoundDirectoryIdentity = serde_json::from_str(stored_parent)
            .map_err(|error| refusal(format!("stored parent identity is invalid: {error}")))?;
        let expected_root: BoundDirectoryIdentity = serde_json::from_str(stored_root)
            .map_err(|error| refusal(format!("stored worktree identity is invalid: {error}")))?;
        if observed_parent != expected_parent || observed_root != expected_root {
            return Err(refusal(
                "bound worktree root or parent identity changed".into(),
            ));
        }
        let expected_marker = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: current_repository_key.clone(),
            repository_root: current_repository_root.clone(),
        };
        self.verify_worktree_root_marker(&root.join(WORKTREE_ROOT_MARKER), &expected_marker)?;
        crate::worktree_registry::verify_exact_registration(
            &self.host_operation_database_path()?,
            root,
            &expected_marker,
            binding.root_source.ok_or_else(|| {
                refusal("bound record has no recorded worktree-root source".into())
            })?,
        )
        .map_err(|error| refusal(format!("cannot verify worktree-root registration: {error}")))?;
        Ok(Some(root.to_path_buf()))
    }

    /// Require a verified durable root before an operation plans or mutates
    /// repository worktree storage. An unbound database remains usable for
    /// operations that do not depend on this root; current config and
    /// environment are never treated as proof of an existing root.
    pub(crate) fn require_bound_repository_worktree_root(
        &self,
        operation: &str,
    ) -> Result<PathBuf, BrokerOpError> {
        use crate::store::repository_binding::RepositoryWorktreeBindingState;

        let binding = self.store.repository_worktree_binding()?;
        let path = binding
            .worktree_root
            .clone()
            .unwrap_or_else(|| self.main_root.join(".aethyme/worktrees"));
        let reason = match binding.state {
            RepositoryWorktreeBindingState::Bound => {
                return self
                    .validate_bound_repository_worktree_root()?
                    .ok_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                        path,
                        reason: format!("{operation} requires a verified worktree-root binding"),
                    });
            }
            RepositoryWorktreeBindingState::LegacyUnbound => {
                "legacy root requires explicit digest-confirmed binding plan/apply"
            }
            RepositoryWorktreeBindingState::FreshUnbound => {
                "fresh root requires an explicit root-creating broker operation"
            }
            RepositoryWorktreeBindingState::FreshReserved
            | RepositoryWorktreeBindingState::RecoveryRequired => {
                "root reservation is incomplete; automatic fallback is disabled. Inspect `aethyme broker advanced worktree-root recovery plan`, then apply only its exact digest-confirmed supported prefix"
            }
        };
        Err(BrokerOpError::WorktreeRootUnavailable {
            path,
            reason: format!("{operation} is unavailable: {reason}"),
        })
    }

    fn build_fresh_root_recovery_plan(
        &self,
        binding: crate::store::repository_binding::RepositoryWorktreeBinding,
        registry: crate::worktree_registry::FreshRootRegistryObservation,
    ) -> Result<FreshRootRecoveryPlan, BrokerOpError> {
        use crate::store::repository_binding::RepositoryWorktreeBindingState;
        use crate::worktree_registry::FreshRootRegistryState;

        let refuse = |path: &Path, reason: String| BrokerOpError::WorktreeRootUnavailable {
            path: path.to_path_buf(),
            reason,
        };
        if !matches!(
            binding.state,
            RepositoryWorktreeBindingState::FreshReserved
                | RepositoryWorktreeBindingState::RecoveryRequired
        ) {
            return Err(refuse(
                &self.main_root,
                format!(
                    "fresh-root recovery requires an incomplete reservation, found {:?}",
                    binding.state
                ),
            ));
        }
        let (identity, root, layout, source, parent_identity) = fresh_root_recovery_identity(
            &binding,
            &self.main_root,
            &self.repo,
            self.store.db_path(),
        )?;
        let marker = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: identity.repository_key.clone(),
            repository_root: identity.repository_root.clone(),
        };
        let root_inventory = fresh_root_recovery_inventory(&root, &marker).map_err(|reason| {
            refuse(
                &root,
                format!("cannot inspect reserved root for recovery: {reason}"),
            )
        })?;
        let (can_apply, refusal) = match (root_inventory.state, registry.state) {
            (
                FreshRootRecoveryPathState::Missing | FreshRootRecoveryPathState::Empty,
                FreshRootRegistryState::MissingDatabase,
            ) => (true, None),
            (
                FreshRootRecoveryPathState::Missing | FreshRootRecoveryPathState::Empty,
                FreshRootRegistryState::ExistingNoTargetRow,
            ) => {
                let available = registry
                    .row_count
                    .is_some_and(|count| count < crate::worktree_registry::MAX_RECORDS);
                if available {
                    (true, None)
                } else {
                    (
                        false,
                        Some("worktree registry is full; recovery will not create the root or prune existing rows".into()),
                    )
                }
            }
            (
                FreshRootRecoveryPathState::Missing | FreshRootRecoveryPathState::Empty,
                FreshRootRegistryState::ExactTargetRow,
            ) => {
                return Err(refuse(
                    &root,
                    "target registry row without its exact ownership marker is not a supported recovery prefix".into(),
                ));
            }
            (FreshRootRecoveryPathState::ExactMarker, FreshRootRegistryState::MissingDatabase) => {
                return Err(refuse(
                    &root,
                    "an exact marker with a missing registry database is not a valid durable prefix".into(),
                ));
            }
            (
                FreshRootRecoveryPathState::ExactMarker,
                FreshRootRegistryState::ExistingNoTargetRow,
            ) => {
                if root_inventory.cargo.is_some() {
                    return Err(refuse(
                        &root,
                        "Cargo defaults cannot precede the durable target registry row".into(),
                    ));
                }
                let available = registry
                    .row_count
                    .is_some_and(|count| count < crate::worktree_registry::MAX_RECORDS);
                if available {
                    (true, None)
                } else {
                    (
                        false,
                        Some(
                            "worktree registry is full; recovery will not prune existing rows"
                                .into(),
                        ),
                    )
                }
            }
            (FreshRootRecoveryPathState::ExactMarker, FreshRootRegistryState::ExactTargetRow) => {
                (true, None)
            }
        };
        let expected_marker_bytes = fresh_root_recovery_marker_bytes(&marker)
            .map_err(|reason| refuse(&root, format!("cannot encode expected marker: {reason}")))?;
        let expected_marker_bytes_sha256 = hex_digest(&expected_marker_bytes);
        let binding_bytes = canonical_json_value_bytes(&binding).map_err(|error| {
            refuse(
                &root,
                format!("cannot encode the exact binding row: {error}"),
            )
        })?;
        let binding_record_sha256 = hex_digest(&binding_bytes);
        let host_operation_database = self.host_operation_database_path()?;
        let mut plan = FreshRootRecoveryPlan {
            schema_version: 1,
            binding,
            repository_root: identity.repository_root,
            repository_key: identity.repository_key,
            broker_database: identity.broker_db_path,
            host_operation_database,
            worktree_root: root,
            layout,
            source,
            parent_identity,
            root_inventory,
            registry,
            expected_marker_bytes_sha256,
            binding_record_sha256,
            can_apply,
            refusal,
            digest: String::new(),
        };
        let digest_input = FreshRootRecoveryDigestInput {
            schema_version: plan.schema_version,
            binding: &plan.binding,
            repository_root: &plan.repository_root,
            repository_key: &plan.repository_key,
            broker_database: &plan.broker_database,
            host_operation_database: &plan.host_operation_database,
            worktree_root: &plan.worktree_root,
            layout: plan.layout,
            source: plan.source,
            parent_identity: &plan.parent_identity,
            root_inventory: &plan.root_inventory,
            registry: &plan.registry,
            expected_marker_bytes_sha256: &plan.expected_marker_bytes_sha256,
            binding_record_sha256: &plan.binding_record_sha256,
            can_apply: plan.can_apply,
            refusal: &plan.refusal,
        };
        let bytes = canonical_json_value_bytes(digest_input).map_err(|error| {
            refuse(
                &plan.worktree_root,
                format!("cannot encode fresh-root recovery plan: {error}"),
            )
        })?;
        plan.digest = hex_digest(&bytes);
        Ok(plan)
    }

    pub(crate) fn fresh_worktree_root_recovery_plan(
        &self,
    ) -> Result<FreshRootRecoveryPlan, BrokerOpError> {
        let binding = self.store.repository_worktree_binding()?;
        let (identity, root, _layout, source, _parent_identity) = fresh_root_recovery_identity(
            &binding,
            &self.main_root,
            &self.repo,
            self.store.db_path(),
        )?;
        let marker = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: identity.repository_key,
            repository_root: identity.repository_root,
        };
        let host_operation_database = self.host_operation_database_path()?;
        let registry = crate::worktree_registry::plan_fresh_root_registry_recovery(
            &host_operation_database,
            &root,
            &marker,
            source,
        )
        .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
            path: root.clone(),
            reason: format!("cannot inspect worktree registry for recovery: {error}"),
        })?;
        self.build_fresh_root_recovery_plan(binding, registry)
    }

    pub(crate) fn apply_fresh_worktree_root_recovery(
        &mut self,
        confirmed_digest: &str,
    ) -> Result<(), BrokerOpError> {
        self.apply_fresh_worktree_root_recovery_with_checkpoint(confirmed_digest, |_, _| Ok(()))
    }

    fn apply_fresh_worktree_root_recovery_with_checkpoint(
        &mut self,
        confirmed_digest: &str,
        checkpoint: impl FnMut(FreshRootRecoveryApplyCheckpoint, &Path) -> Result<(), String>,
    ) -> Result<(), BrokerOpError> {
        self.apply_fresh_worktree_root_recovery_with_checkpoint_and_sync(
            confirmed_digest,
            checkpoint,
            sync_bound_directory,
        )
    }

    fn apply_fresh_worktree_root_recovery_with_checkpoint_and_sync(
        &mut self,
        confirmed_digest: &str,
        checkpoint: impl FnMut(FreshRootRecoveryApplyCheckpoint, &Path) -> Result<(), String>,
        sync_directory: impl FnMut(&Path, &BoundDirectoryIdentity) -> Result<(), String>,
    ) -> Result<(), BrokerOpError> {
        self.apply_fresh_worktree_root_recovery_with_checkpoint_and_sync_and_marker_file(
            confirmed_digest,
            checkpoint,
            sync_directory,
            |root, marker, expected_root, expected_file| {
                sync_fresh_root_recovery_marker_file(root, marker, expected_root, expected_file)
            },
        )
    }

    fn apply_fresh_worktree_root_recovery_with_checkpoint_and_sync_and_marker_file(
        &mut self,
        confirmed_digest: &str,
        mut checkpoint: impl FnMut(FreshRootRecoveryApplyCheckpoint, &Path) -> Result<(), String>,
        mut sync_directory: impl FnMut(&Path, &BoundDirectoryIdentity) -> Result<(), String>,
        mut sync_marker_file: impl FnMut(
            &Path,
            &WorktreeRootMarker,
            &BoundDirectoryIdentity,
            &crate::worktree_registry::RecoveryArtifactIdentity,
        ) -> Result<(), String>,
    ) -> Result<(), BrokerOpError> {
        let _binding_lock =
            crate::store::fresh_database::acquire_repository_binding_lock_for_explicit_recovery(
                &self.main_root,
                self.store.db_path(),
            )?;
        let binding = self.store.repository_worktree_binding()?;
        let (identity, root, _layout, source, parent_identity) = fresh_root_recovery_identity(
            &binding,
            &self.main_root,
            &self.repo,
            self.store.db_path(),
        )?;
        let marker = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: identity.repository_key.clone(),
            repository_root: identity.repository_root.clone(),
        };
        let host_operation_database = self.host_operation_database_path()?;
        let mut registry_guard = crate::worktree_registry::begin_fresh_root_registry_recovery(
            &host_operation_database,
            &root,
            &marker,
            source,
        )
        .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
            path: root.clone(),
            reason: format!("cannot lock and inspect worktree registry for recovery: {error}"),
        })?;
        let plan = self.build_fresh_root_recovery_plan(
            binding.clone(),
            registry_guard.observation().clone(),
        )?;
        if plan.digest != confirmed_digest {
            return Err(BrokerOpError::Store(
                crate::error::BrokerError::RepositoryWorktreeBindingConfirmationMismatch,
            ));
        }
        if !plan.can_apply {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: root,
                reason: plan
                    .refusal
                    .unwrap_or_else(|| "fresh-root recovery plan is not applicable".into()),
            });
        }
        if plan.registry.state != crate::worktree_registry::FreshRootRegistryState::MissingDatabase
        {
            registry_guard
                .check_capacity_before_marker()
                .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
                    path: plan.worktree_root.clone(),
                    reason: format!("recovery preflight refused before root writes: {error}"),
                })?;
        }
        let current_inventory = fresh_root_recovery_inventory(&plan.worktree_root, &marker)
            .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("reserved root changed after plan review: {reason}"),
            })?;
        if current_inventory != plan.root_inventory {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: "reserved root inventory changed after plan review".into(),
            });
        }

        let mut expected_root_identity = plan.root_inventory.root_identity.clone();
        if plan.root_inventory.state == FreshRootRecoveryPathState::Missing {
            let parent = plan.worktree_root.parent().ok_or_else(|| {
                BrokerOpError::WorktreeRootUnavailable {
                    path: plan.worktree_root.clone(),
                    reason: "reserved root has no parent".into(),
                }
            })?;
            if bound_directory_identity(parent).as_ref() != Ok(&parent_identity) {
                return Err(BrokerOpError::WorktreeRootUnavailable {
                    path: plan.worktree_root.clone(),
                    reason: "reserved root parent identity changed before creation".into(),
                });
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = std::fs::DirBuilder::new();
                builder.mode(0o700);
                builder.create(&plan.worktree_root).map_err(|error| {
                    BrokerOpError::WorktreeRootUnavailable {
                        path: plan.worktree_root.clone(),
                        reason: format!("cannot create reserved root without replacement: {error}"),
                    }
                })?;
            }
            #[cfg(not(unix))]
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: "fresh-root recovery requires Unix no-follow directory creation".into(),
            });
            let created_identity =
                bound_directory_identity(&plan.worktree_root).map_err(|reason| {
                    BrokerOpError::WorktreeRootUnavailable {
                        path: plan.worktree_root.clone(),
                        reason: format!(
                            "cannot capture newly created recovery root identity: {reason}"
                        ),
                    }
                })?;
            crate::host_state::protect_host_state_path(&plan.worktree_root, true).map_err(
                |error| BrokerOpError::WorktreeRootUnavailable {
                    path: plan.worktree_root.clone(),
                    reason: format!("cannot protect recovered root: {error}"),
                },
            )?;
            verify_private_worktree_root_permissions(&plan.worktree_root).map_err(|reason| {
                BrokerOpError::WorktreeRootUnavailable {
                    path: plan.worktree_root.clone(),
                    reason: format!("cannot verify recovered root permissions: {reason}"),
                }
            })?;
            require_bound_directory_identity(&plan.worktree_root, &created_identity).map_err(
                |reason| BrokerOpError::WorktreeRootUnavailable {
                    path: plan.worktree_root.clone(),
                    reason: format!(
                        "newly created recovery root changed during protection: {reason}"
                    ),
                },
            )?;
            expected_root_identity = Some(created_identity);
            sync_directory(parent, &parent_identity).map_err(|reason| {
                BrokerOpError::WorktreeRootUnavailable {
                    path: parent.to_path_buf(),
                    reason: format!("cannot sync recovered root name: {reason}"),
                }
            })?;
        }

        let root_identity =
            expected_root_identity.ok_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: "reviewed recovery root has no exact directory identity".into(),
            })?;
        let parent =
            plan.worktree_root
                .parent()
                .ok_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                    path: plan.worktree_root.clone(),
                    reason: "reserved root has no parent".into(),
                })?;
        sync_directory(parent, &parent_identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: parent.to_path_buf(),
                reason: format!("cannot sync reviewed recovery root parent: {reason}"),
            }
        })?;
        checkpoint(
            FreshRootRecoveryApplyCheckpoint::BeforeRegistryInitialization,
            &plan.worktree_root,
        )
        .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
            path: plan.worktree_root.clone(),
            reason: format!("recovery checkpoint refused before registry initialization: {reason}"),
        })?;
        require_bound_directory_identity(&plan.worktree_root, &root_identity).map_err(
            |reason| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("reserved root changed before registry initialization: {reason}"),
            },
        )?;

        if registry_guard.observation().state
            == crate::worktree_registry::FreshRootRegistryState::MissingDatabase
        {
            registry_guard
                .ensure_initialized_after_root()
                .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
                    path: plan.worktree_root.clone(),
                    reason: format!(
                        "cannot durably initialize the missing recovery registry: {error}"
                    ),
                })?;
        }
        require_bound_directory_identity(&plan.worktree_root, &root_identity).map_err(
            |reason| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("reserved root changed during registry initialization: {reason}"),
            },
        )?;
        registry_guard
            .check_capacity_before_marker()
            .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("recovery capacity check failed before marker creation: {error}"),
            })?;
        require_bound_directory_identity(&plan.worktree_root, &root_identity).map_err(
            |reason| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("reserved root changed during recovery preflight: {reason}"),
            },
        )?;
        checkpoint(
            FreshRootRecoveryApplyCheckpoint::AfterRegistryInitializationBeforeMarker,
            &plan.worktree_root,
        )
        .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
            path: plan.worktree_root.clone(),
            reason: format!("recovery checkpoint refused after registry initialization: {reason}"),
        })?;
        require_bound_directory_identity(&plan.worktree_root, &root_identity).map_err(
            |reason| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("reserved root changed after registry initialization: {reason}"),
            },
        )?;

        let root_before_marker = fresh_root_recovery_inventory(&plan.worktree_root, &marker)
            .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("cannot recheck recovery root before marker creation: {reason}"),
            })?;
        if plan.root_inventory.state == FreshRootRecoveryPathState::ExactMarker {
            if root_before_marker != plan.root_inventory {
                return Err(BrokerOpError::WorktreeRootUnavailable {
                    path: plan.worktree_root.clone(),
                    reason: "exact marker root changed before registry completion".into(),
                });
            }
        } else if root_before_marker.state != FreshRootRecoveryPathState::Empty
            || root_before_marker.cargo.is_some()
            || root_before_marker.root_identity.as_ref() != Some(&root_identity)
        {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: "unmarked root identity or inventory changed before marker creation".into(),
            });
        }

        if plan.root_inventory.state == FreshRootRecoveryPathState::ExactMarker
            && let Some(cargo_inventory) = &plan.root_inventory.cargo
        {
            normalize_fresh_root_recovery_cargo(
                &plan.worktree_root,
                &root_identity,
                cargo_inventory,
            )
            .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.join(".cargo"),
                reason: format!("cannot finalize reviewed Cargo defaults before binding: {reason}"),
            })?;
        }

        let marker_file_identity =
            if plan.root_inventory.state == FreshRootRecoveryPathState::ExactMarker {
                plan.root_inventory.marker_identity.clone().ok_or_else(|| {
                    BrokerOpError::WorktreeRootUnavailable {
                        path: plan.worktree_root.join(WORKTREE_ROOT_MARKER),
                        reason: "reviewed exact marker has no file identity".into(),
                    }
                })?
            } else {
                create_fresh_root_recovery_marker(&plan.worktree_root, &marker, &root_identity)
                    .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
                        path: plan.worktree_root.join(WORKTREE_ROOT_MARKER),
                        reason,
                    })?
            };
        require_bound_directory_identity(&plan.worktree_root, &root_identity).map_err(
            |reason| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("reserved root changed after marker publication: {reason}"),
            },
        )?;
        let marker_inventory = fresh_root_recovery_inventory(&plan.worktree_root, &marker)
            .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("cannot verify published recovery marker: {reason}"),
            })?;
        if marker_inventory.state != FreshRootRecoveryPathState::ExactMarker
            || marker_inventory.root_identity.as_ref() != Some(&root_identity)
            || marker_inventory.marker_identity.as_ref() != Some(&marker_file_identity)
        {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: "published marker does not belong to the reviewed recovery root".into(),
            });
        }
        sync_marker_file(
            &plan.worktree_root,
            &marker,
            &root_identity,
            &marker_file_identity,
        )
        .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
            path: plan.worktree_root.join(WORKTREE_ROOT_MARKER),
            reason: format!("cannot durably sync recovery marker file: {reason}"),
        })?;
        let synced_marker_inventory = fresh_root_recovery_inventory(&plan.worktree_root, &marker)
            .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
            path: plan.worktree_root.clone(),
            reason: format!("cannot verify recovery marker after file sync: {reason}"),
        })?;
        if synced_marker_inventory.state != FreshRootRecoveryPathState::ExactMarker
            || synced_marker_inventory.root_identity.as_ref() != Some(&root_identity)
            || synced_marker_inventory.marker_identity.as_ref() != Some(&marker_file_identity)
        {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: "recovery marker changed during file sync".into(),
            });
        }
        sync_directory(&plan.worktree_root, &root_identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("cannot durably sync recovery marker namespace: {reason}"),
            }
        })?;
        checkpoint(
            FreshRootRecoveryApplyCheckpoint::MarkerDurablyPublishedBeforeRegistryRecord,
            &plan.worktree_root,
        )
        .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
            path: plan.worktree_root.clone(),
            reason: format!(
                "recovery stopped after marker durability and before registry record: {reason}"
            ),
        })?;
        require_bound_directory_identity(&plan.worktree_root, &root_identity).map_err(
            |reason| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("reserved root changed before registry record: {reason}"),
            },
        )?;
        registry_guard.record_exact_target().map_err(|error| {
            BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("cannot durably record recovered worktree root: {error}"),
            }
        })?;
        require_bound_directory_identity(&plan.worktree_root, &root_identity).map_err(
            |reason| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("reserved root changed after registry record: {reason}"),
            },
        )?;
        let recorded_inventory = fresh_root_recovery_inventory(&plan.worktree_root, &marker)
            .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("cannot verify recovery root after registry record: {reason}"),
            })?;
        if recorded_inventory.state != FreshRootRecoveryPathState::ExactMarker
            || recorded_inventory.root_identity.as_ref() != Some(&root_identity)
            || registry_guard.observation().state
                != crate::worktree_registry::FreshRootRegistryState::ExactTargetRow
        {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: "durable registry row does not match the reviewed root marker and identity"
                    .into(),
            });
        }
        checkpoint(
            FreshRootRecoveryApplyCheckpoint::RegistryRowDurablyRecordedBeforeBinding,
            &plan.worktree_root,
        )
        .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
            path: plan.worktree_root.clone(),
            reason: format!(
                "recovery stopped after registry durability and before binding: {reason}"
            ),
        })?;
        require_bound_directory_identity(&plan.worktree_root, &root_identity).map_err(
            |reason| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("reserved root changed before binding completion: {reason}"),
            },
        )?;
        let parent =
            plan.worktree_root
                .parent()
                .ok_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                    path: plan.worktree_root.clone(),
                    reason: "reserved root has no parent".into(),
                })?;
        require_bound_directory_identity(parent, &parent_identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: parent.to_path_buf(),
                reason: format!("reserved root parent changed before binding completion: {reason}"),
            }
        })?;
        sync_directory(parent, &parent_identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: parent.to_path_buf(),
                reason: format!("cannot sync recovered root parent before binding: {reason}"),
            }
        })?;
        require_bound_directory_identity(parent, &parent_identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: parent.to_path_buf(),
                reason: format!(
                    "reserved root parent changed while syncing before binding: {reason}"
                ),
            }
        })?;
        let root_identity_json = serde_json::to_string(&root_identity).map_err(|error| {
            BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: format!("cannot encode recovered root identity: {error}"),
            }
        })?;
        let reservation_digest = binding.reservation_digest.as_deref().ok_or_else(|| {
            BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root.clone(),
                reason: "fresh reservation has no digest".into(),
            }
        })?;
        self.store.complete_fresh_repository_worktree_binding(
            &identity,
            reservation_digest,
            &root_identity_json,
        )?;
        self.validate_bound_repository_worktree_root()?
            .ok_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                path: plan.worktree_root,
                reason: "recovered root completed binding but did not validate".into(),
            })?;
        Ok(())
    }

    pub(crate) fn legacy_worktree_root_binding_plan(
        &self,
        requested_root: &Path,
        layout: crate::store::repository_binding::RepositoryWorktreeRootLayout,
    ) -> Result<LegacyWorktreeRootBindingPlan, BrokerOpError> {
        use crate::store::repository_binding::RepositoryWorktreeBindingState;

        let refusal = |path: &Path, reason: String| BrokerOpError::WorktreeRootUnavailable {
            path: path.to_path_buf(),
            reason,
        };
        let binding = self.store.repository_worktree_binding()?;
        if binding.state != RepositoryWorktreeBindingState::LegacyUnbound {
            return Err(refusal(
                requested_root,
                format!(
                    "explicit legacy adoption requires legacy_unbound state, found {:?}",
                    binding.state
                ),
            ));
        }
        let root = requested_root
            .canonicalize()
            .map_err(|error| refusal(requested_root, format!("cannot resolve root: {error}")))?;
        let root_identity = bound_directory_identity(&root)
            .map_err(|reason| refusal(&root, format!("cannot verify legacy root: {reason}")))?;
        verify_private_worktree_root_permissions(&root).map_err(|reason| {
            refusal(
                &root,
                format!("cannot verify legacy root permissions: {reason}"),
            )
        })?;
        let parent = root
            .parent()
            .ok_or_else(|| refusal(&root, "legacy root has no parent".into()))?;
        let parent_identity = bound_directory_identity(parent).map_err(|reason| {
            refusal(
                parent,
                format!("cannot verify legacy root parent: {reason}"),
            )
        })?;
        let repository_root = self.main_root.canonicalize().map_err(|error| {
            refusal(&self.main_root, format!("cannot verify checkout: {error}"))
        })?;
        let git_common_dir = self
            .main_git_common_dir()?
            .canonicalize()
            .map_err(|error| {
                refusal(
                    &self.main_root,
                    format!("cannot verify Git identity: {error}"),
                )
            })?;
        let repository_key =
            crate::host_state::repository_key(&repository_root, Some(&git_common_dir));
        let broker_db_path = self.store.db_path().canonicalize().map_err(|error| {
            refusal(
                self.store.db_path(),
                format!("cannot verify broker database: {error}"),
            )
        })?;
        let expected_marker = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: repository_key.clone(),
            repository_root: repository_root.clone(),
        };
        self.verify_worktree_root_marker(&root.join(WORKTREE_ROOT_MARKER), &expected_marker)?;
        let source = crate::worktree_registry::registered_source(
            &self.host_operation_database_path()?,
            &root,
            &expected_marker,
        )
        .map_err(|error| {
            refusal(
                &root,
                format!("cannot verify retained root registration: {error}"),
            )
        })?;
        let parent_json = serde_json::to_string(&parent_identity)
            .map_err(|error| refusal(parent, format!("cannot encode parent identity: {error}")))?;
        let worktree_json = serde_json::to_string(&root_identity)
            .map_err(|error| refusal(&root, format!("cannot encode root identity: {error}")))?;
        let layout_name = match layout {
            crate::store::repository_binding::RepositoryWorktreeRootLayout::KeyedContainer => {
                "keyed_container"
            }
            crate::store::repository_binding::RepositoryWorktreeRootLayout::DirectRoot => {
                "direct_root"
            }
        };
        let digest = legacy_worktree_root_binding_digest(LegacyWorktreeRootBindingDigestInput {
            repository_root: &repository_root,
            git_common_dir: &git_common_dir,
            repository_key: &repository_key,
            broker_db_path: &broker_db_path,
            worktree_root: &root,
            layout: layout_name,
            source,
            parent_identity: &parent_json,
            worktree_identity: &worktree_json,
        })
        .map_err(|error| refusal(&root, format!("cannot encode legacy binding plan: {error}")))?;
        Ok(LegacyWorktreeRootBindingPlan {
            schema_version: 1,
            repository_root,
            git_common_dir,
            repository_key,
            broker_db_path,
            worktree_root: root,
            layout: layout_name.to_owned(),
            source,
            parent_identity,
            worktree_identity: root_identity,
            digest,
        })
    }

    pub(crate) fn apply_legacy_worktree_root_binding(
        &mut self,
        requested_root: &Path,
        layout: crate::store::repository_binding::RepositoryWorktreeRootLayout,
        confirmed_digest: &str,
    ) -> Result<(), BrokerOpError> {
        use crate::store::repository_binding::{
            LegacyRepositoryRootConfirmation, RepositoryWorktreeBindingState,
        };

        let _bootstrap_lock =
            crate::store::fresh_database::acquire_repository_binding_lock_for_explicit_recovery(
                &self.main_root,
                self.store.db_path(),
            )?;
        let binding = self.store.repository_worktree_binding()?;
        if binding.state != RepositoryWorktreeBindingState::LegacyUnbound {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: requested_root.to_path_buf(),
                reason: format!(
                    "explicit legacy adoption requires legacy_unbound state, found {:?}",
                    binding.state
                ),
            });
        }
        let plan = self.legacy_worktree_root_binding_plan(requested_root, layout)?;
        if plan.digest != confirmed_digest {
            return Err(BrokerOpError::Store(
                crate::error::BrokerError::RepositoryWorktreeBindingConfirmationMismatch,
            ));
        }
        let confirmation = LegacyRepositoryRootConfirmation {
            repository_root: plan.repository_root,
            git_common_dir: plan.git_common_dir,
            repository_key: plan.repository_key,
            broker_db_path: plan.broker_db_path,
            worktree_root: plan.worktree_root,
            layout,
            source: plan.source,
            plan_digest: plan.digest,
            confirmed_digest: confirmed_digest.to_owned(),
            parent_identity_json: serde_json::to_string(&plan.parent_identity).map_err(
                |error| BrokerOpError::WorktreeRootUnavailable {
                    path: requested_root.to_path_buf(),
                    reason: format!("cannot encode parent identity: {error}"),
                },
            )?,
            worktree_identity_json: serde_json::to_string(&plan.worktree_identity).map_err(
                |error| BrokerOpError::WorktreeRootUnavailable {
                    path: requested_root.to_path_buf(),
                    reason: format!("cannot encode root identity: {error}"),
                },
            )?,
        };
        self.store.bind_legacy_repository_worktree(&confirmation)?;
        self.validate_bound_repository_worktree_root()?
            .ok_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                path: requested_root.to_path_buf(),
                reason: "legacy root binding completed but did not validate".into(),
            })?;
        Ok(())
    }

    /// Submission may create a root only after its non-root checks have
    /// passed. A fresh-unbound database can continue through preflight;
    /// legacy and incomplete bindings keep the existing refusal, and a bound
    /// root is validated immediately.
    pub(crate) fn check_submission_root_before_planning(&self) -> Result<(), BrokerOpError> {
        use crate::store::repository_binding::RepositoryWorktreeBindingState;

        if self.store.repository_worktree_binding()?.state
            == RepositoryWorktreeBindingState::FreshUnbound
        {
            return Ok(());
        }
        self.require_bound_repository_worktree_root("session submission")?;
        Ok(())
    }

    /// Submission is an explicit root-creating operation. Keep open, status,
    /// and adoption observational for an unbound repository; prepare a fresh
    /// root only after session, checkout, policy, and plan validation.
    pub(crate) fn prepare_worktree_root_for_submission(&mut self) -> Result<(), BrokerOpError> {
        use crate::store::repository_binding::RepositoryWorktreeBindingState;

        if self.store.repository_worktree_binding()?.state
            == RepositoryWorktreeBindingState::FreshUnbound
        {
            self.prepare_broker_worktree_root()?;
        }
        self.require_bound_repository_worktree_root("session submission")?;
        Ok(())
    }

    pub(super) fn prepare_broker_worktree_root(
        &mut self,
    ) -> Result<WorktreePlacement, BrokerOpError> {
        use crate::store::repository_binding::RepositoryWorktreeBindingState;

        let binding = self.store.repository_worktree_binding()?;
        match binding.state {
            RepositoryWorktreeBindingState::Bound => {
                let root = self
                    .validate_bound_repository_worktree_root()?
                    .ok_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                        path: self.main_root.clone(),
                        reason: "bound root identity is unavailable".into(),
                    })?;
                return Ok(WorktreePlacement {
                    root,
                    source: binding.root_source.ok_or_else(|| {
                        BrokerOpError::WorktreeRootUnavailable {
                            path: self.main_root.clone(),
                            reason: "bound record has no worktree-root source".into(),
                        }
                    })?,
                    outside_repository: binding.root_source
                        != Some(WorktreeRootSource::RepositoryFallback),
                    fallback_reason: None,
                });
            }
            RepositoryWorktreeBindingState::LegacyUnbound => {
                return Err(BrokerOpError::WorktreeRootUnavailable {
                    path: self.main_root.join(".aethyme/worktrees"),
                    reason: "legacy repository has no reviewed worktree-root binding; inspect `aethyme broker advanced worktree-root`, then use the explicit binding plan/apply workflow".into(),
                });
            }
            RepositoryWorktreeBindingState::FreshReserved
            | RepositoryWorktreeBindingState::RecoveryRequired => {
                return Err(BrokerOpError::WorktreeRootUnavailable {
                    path: binding.worktree_root.unwrap_or_else(|| self.main_root.clone()),
                    reason: "fresh worktree-root reservation is incomplete; automatic fallback is disabled. Inspect `aethyme broker advanced worktree-root recovery plan`, then apply only its exact digest-confirmed supported prefix".into(),
                });
            }
            RepositoryWorktreeBindingState::FreshUnbound => {}
        }

        let plan = self.worktree_root_plan()?;
        let candidates = worktree_root_candidates_for(&plan);
        let strict_override = matches!(
            plan.preferred_source,
            Some(WorktreeRootSource::EnvironmentOverride | WorktreeRootSource::LibraryOverride)
        );
        let mut fallback_reason = if plan.preferred_source
            == Some(WorktreeRootSource::RepositoryConfig)
            && let Some(reason) = &plan.preferred_unavailable_reason
        {
            Some(format!("configured worktree root is unavailable: {reason}"))
        } else if plan.preferred_source.is_none() {
            Some(
                "no per-user host-state directory is available; set AETHYME_WORKTREE_ROOT to a writable external base"
                    .into(),
            )
        } else {
            plan.preferred_unavailable_reason.clone()
        };
        let mut last_error = None;
        for (path, source) in candidates {
            let bootstrap_lock =
                match self.prepare_missing_default_fresh_root_parent(&plan, &path, source) {
                    Ok(lock) => lock,
                    Err(error) if strict_override => return Err(error),
                    Err(error) => {
                        let reason = match source {
                            WorktreeRootSource::RepositoryConfig => {
                                format!("configured worktree root is unavailable: {error}")
                            }
                            WorktreeRootSource::HostState
                                if plan.preferred_source
                                    == Some(WorktreeRootSource::RepositoryConfig) =>
                            {
                                fallback_reason.unwrap_or_else(|| error.to_string())
                            }
                            _ => error.to_string(),
                        };
                        fallback_reason = Some(reason);
                        last_error = Some(error);
                        continue;
                    }
                };
            let candidate = match self.preflight_fresh_worktree_root(&plan, &path, source) {
                Ok(candidate) => candidate,
                Err(error) if strict_override => return Err(error),
                Err(error) => {
                    let reason = match source {
                        WorktreeRootSource::RepositoryConfig => {
                            format!("configured worktree root is unavailable: {error}")
                        }
                        WorktreeRootSource::HostState
                            if plan.preferred_source
                                == Some(WorktreeRootSource::RepositoryConfig) =>
                        {
                            fallback_reason.unwrap_or_else(|| error.to_string())
                        }
                        _ => error.to_string(),
                    };
                    fallback_reason = Some(reason);
                    last_error = Some(error);
                    continue;
                }
            };
            return self.reserve_and_create_fresh_worktree_root(
                candidate,
                fallback_reason,
                bootstrap_lock,
            );
        }
        Err(
            last_error.unwrap_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                path: plan.legacy_fallback_root,
                reason: "no worktree-root candidates are available".into(),
            }),
        )
    }

    fn preflight_fresh_worktree_root(
        &self,
        plan: &WorktreeRootPlan,
        requested_root: &Path,
        source: WorktreeRootSource,
    ) -> Result<FreshWorktreeRootCandidate, BrokerOpError> {
        let name =
            requested_root
                .file_name()
                .ok_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                    path: requested_root.to_path_buf(),
                    reason: "worktree root path has no final component".into(),
                })?;
        let requested_parent =
            requested_root
                .parent()
                .ok_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                    path: requested_root.to_path_buf(),
                    reason: "worktree root path has no parent".into(),
                })?;
        let parent = requested_parent.canonicalize().map_err(|error| {
            BrokerOpError::WorktreeRootUnavailable {
                path: requested_parent.to_path_buf(),
                reason: format!("fresh binding requires an existing root parent: {error}"),
            }
        })?;
        let parent_identity = bound_directory_identity(&parent).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: parent.clone(),
                reason: format!("cannot verify root parent before reservation: {reason}"),
            }
        })?;
        let root = parent.join(name);
        match std::fs::symlink_metadata(&root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(BrokerOpError::WorktreeRootUnavailable {
                    path: root,
                    reason: "fresh binding refuses an existing root; legacy roots require explicit digest-confirmed adoption".into(),
                });
            }
            Err(error) => {
                return Err(BrokerOpError::WorktreeRootUnavailable {
                    path: root,
                    reason: format!("cannot inspect fresh root candidate: {error}"),
                });
            }
        }
        let require_external = source != WorktreeRootSource::RepositoryFallback;
        let min_free_bytes = self.configured_worktree_minimum(source)?;
        preflight_worktree_root(
            &self.main_root,
            &root,
            source,
            require_external,
            min_free_bytes,
            crate::verification::removable_volume_unavailable_reason,
            |candidate| {
                crate::disk_headroom::available_headroom_at_or_above_for(&self.main_root, candidate)
                    .map(|headroom| headroom.bytes)
            },
        )
        .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
            path: root.clone(),
            reason,
        })?;
        // A fresh root beneath another linked checkout would itself be a
        // write inside that checkout. Refuse it before reservation creates
        // the directory or ownership marker; the later destination check
        // still protects the actual worktree path.
        self.refuse_nested_worktree_path(&root)?;
        let layout = if source != WorktreeRootSource::RepositoryFallback
            && source != WorktreeRootSource::LibraryOverride
            && (plan
                .root_container
                .as_deref()
                .is_some_and(|container| container == parent)
                || plan
                    .host_state_fallback_root
                    .as_deref()
                    .is_some_and(|fallback| fallback == root))
        {
            crate::store::repository_binding::RepositoryWorktreeRootLayout::KeyedContainer
        } else {
            crate::store::repository_binding::RepositoryWorktreeRootLayout::DirectRoot
        };
        Ok(FreshWorktreeRootCandidate {
            root,
            parent_identity_json: serde_json::to_string(&parent_identity).map_err(|error| {
                BrokerOpError::WorktreeRootUnavailable {
                    path: parent,
                    reason: format!("cannot encode root-parent identity: {error}"),
                }
            })?,
            layout,
            source,
        })
    }

    /// Create only a missing, standard parent for an explicit fresh-root
    /// operation. Repository fallback may create `.aethyme`; a host-state
    /// candidate may create `worktrees` only beneath an existing captured
    /// host-state directory. Overrides and arbitrary configured roots remain
    /// strict, and an absent host-state base is never synthesized here.
    fn prepare_missing_default_fresh_root_parent(
        &self,
        plan: &WorktreeRootPlan,
        requested_root: &Path,
        source: WorktreeRootSource,
    ) -> Result<Option<std::fs::File>, BrokerOpError> {
        let refusal = |path: &Path, reason: String| BrokerOpError::WorktreeRootUnavailable {
            path: path.to_path_buf(),
            reason,
        };

        let (parent, ancestor) = match source {
            WorktreeRootSource::RepositoryFallback => {
                let expected_parent = self.main_root.join(".aethyme");
                if requested_root != plan.legacy_fallback_root
                    || requested_root.parent() != Some(expected_parent.as_path())
                {
                    return Ok(None);
                }
                (expected_parent, self.main_root.as_path())
            }
            WorktreeRootSource::HostState => {
                let Some(host_state) = self.storage_context.host_state_dir.as_deref() else {
                    return Ok(None);
                };
                let parent = host_state.join("worktrees");
                let expected_root = parent.join(&plan.repository_key);
                if requested_root != expected_root {
                    return Ok(None);
                }
                let host_state_metadata = match std::fs::symlink_metadata(host_state) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(None);
                    }
                    Err(error) => {
                        return Err(refusal(
                            host_state,
                            format!("cannot inspect captured host-state parent: {error}"),
                        ));
                    }
                };
                if host_state_metadata.file_type().is_symlink() || !host_state_metadata.is_dir() {
                    return Err(refusal(
                        host_state,
                        "captured host-state parent is not a real directory".into(),
                    ));
                }
                (parent, host_state)
            }
            _ => return Ok(None),
        };

        match std::fs::symlink_metadata(&parent) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(refusal(
                        &parent,
                        "standard worktree-root parent is not a real directory".into(),
                    ));
                }
                bound_directory_identity(&parent).map_err(|reason| {
                    refusal(
                        &parent,
                        format!("cannot verify standard worktree-root parent: {reason}"),
                    )
                })?;
                return Ok(None);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(refusal(
                    &parent,
                    format!("cannot inspect standard worktree-root parent: {error}"),
                ));
            }
        }

        let ancestor_identity = match bound_directory_identity(ancestor) {
            Ok(identity) => identity,
            Err(_reason) if source == WorktreeRootSource::HostState => {
                // A missing or redirected host-state base is not created or
                // repaired by worktree placement; the repository fallback
                // remains available to the candidate loop.
                return Ok(None);
            }
            Err(reason) => {
                return Err(refusal(
                    ancestor,
                    format!("cannot verify parent of standard worktree root: {reason}"),
                ));
            }
        };

        // A host-state base can be configured inside another linked checkout.
        // Refuse that owner before creating its standard worktrees parent.
        self.refuse_nested_worktree_path(&ancestor_identity.canonical_path)?;
        let minimum = self.configured_worktree_minimum(source)?;
        preflight_worktree_root(
            &self.main_root,
            requested_root,
            source,
            source != WorktreeRootSource::RepositoryFallback,
            minimum,
            crate::verification::removable_volume_unavailable_reason,
            |candidate| {
                crate::disk_headroom::available_headroom_at_or_above_for(&self.main_root, candidate)
                    .map(|headroom| headroom.bytes)
            },
        )
        .map_err(|reason| refusal(requested_root, reason))?;

        let bootstrap_lock = crate::store::fresh_database::acquire_repository_binding_lock(
            &self.main_root,
            self.store.db_path(),
        )?;
        let binding = self.store.repository_worktree_binding()?;
        fresh_root_identity(&binding, &self.main_root, &self.repo, self.store.db_path())?;
        if bound_directory_identity(ancestor).as_ref() != Ok(&ancestor_identity) {
            return Err(refusal(
                ancestor,
                "parent of standard worktree root changed before creation".into(),
            ));
        }
        match std::fs::symlink_metadata(&parent) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(refusal(
                    &parent,
                    "standard worktree-root parent appeared before exclusive creation".into(),
                ));
            }
            Err(error) => {
                return Err(refusal(
                    &parent,
                    format!("cannot recheck standard worktree-root parent: {error}"),
                ));
            }
        }
        // This is only the standard parent container. A crash before the
        // reservation may leave it empty; no root identity is inferred from
        // that directory, and the next explicit start revalidates it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(&parent).map_err(|error| {
                refusal(
                    &parent,
                    format!("cannot create standard worktree-root parent: {error}"),
                )
            })?;
        }
        #[cfg(not(unix))]
        {
            return Err(refusal(
                &parent,
                "safe fresh worktree-root parent creation requires Unix no-follow checks".into(),
            ));
        }
        let parent_identity = bound_directory_identity(&parent).map_err(|reason| {
            refusal(&parent, format!("cannot verify created parent: {reason}"))
        })?;
        // This directory was created by the current process and must remain
        // owned by the same user as the verified parent.
        if parent_identity.uid != ancestor_identity.uid {
            return Err(refusal(
                &parent,
                "created worktree-root parent has a different owner".into(),
            ));
        }
        sync_bound_directory(&parent, &parent_identity)
            .map_err(|reason| refusal(&parent, format!("cannot sync created parent: {reason}")))?;
        sync_bound_directory(ancestor, &ancestor_identity)
            .map_err(|reason| refusal(ancestor, format!("cannot sync parent name: {reason}")))?;
        Ok(Some(bootstrap_lock))
    }

    fn reserve_and_create_fresh_worktree_root(
        &mut self,
        candidate: FreshWorktreeRootCandidate,
        fallback_reason: Option<String>,
        bootstrap_lock: Option<std::fs::File>,
    ) -> Result<WorktreePlacement, BrokerOpError> {
        use crate::store::repository_binding::{
            FreshRepositoryRootReservation, RepositoryWorktreeBindingOrigin,
            RepositoryWorktreeBindingState,
        };

        let binding = self.store.repository_worktree_binding()?;
        let identity =
            fresh_root_identity(&binding, &self.main_root, &self.repo, self.store.db_path())?;
        let reservation_digest = fresh_root_reservation_digest(
            &identity,
            &candidate.root,
            candidate.layout,
            candidate.source,
            &candidate.parent_identity_json,
        )?;
        let reservation = FreshRepositoryRootReservation {
            identity: identity.clone(),
            worktree_root: candidate.root.clone(),
            layout: candidate.layout,
            source: candidate.source,
            reservation_digest: reservation_digest.clone(),
            parent_identity_json: candidate.parent_identity_json.clone(),
        };
        let _bootstrap_lock = match bootstrap_lock {
            Some(lock) => lock,
            None => crate::store::fresh_database::acquire_repository_binding_lock(
                &self.main_root,
                self.store.db_path(),
            )?,
        };
        let current = self.store.repository_worktree_binding()?;
        if current.state != RepositoryWorktreeBindingState::FreshUnbound
            || current.origin != Some(RepositoryWorktreeBindingOrigin::Fresh)
            || current.repository_root.as_deref() != Some(identity.repository_root.as_path())
            || current.git_common_dir.as_deref() != Some(identity.git_common_dir.as_path())
            || current.repository_key.as_deref() != Some(identity.repository_key.as_str())
            || current.broker_db_path.as_deref() != Some(identity.broker_db_path.as_path())
            || current.bootstrap_nonce.as_deref() != Some(identity.bootstrap_nonce.as_str())
        {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: candidate.root,
                reason: "fresh database binding changed before root reservation".into(),
            });
        }
        let parent =
            candidate
                .root
                .parent()
                .ok_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                    path: candidate.root.clone(),
                    reason: "reserved root has no parent".into(),
                })?;
        let expected_parent: BoundDirectoryIdentity =
            serde_json::from_str(&candidate.parent_identity_json).map_err(|error| {
                BrokerOpError::WorktreeRootUnavailable {
                    path: parent.to_path_buf(),
                    reason: format!("reserved parent identity is invalid: {error}"),
                }
            })?;
        let actual_parent = bound_directory_identity(parent).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: parent.to_path_buf(),
                reason: format!("root parent changed before reservation: {reason}"),
            }
        })?;
        if actual_parent != expected_parent {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: parent.to_path_buf(),
                reason: "root parent identity changed before reservation".into(),
            });
        }
        let registration_admission =
            self.admit_fresh_worktree_root_registration(&candidate.root)?;
        self.store
            .reserve_fresh_repository_worktree_root(&reservation)?;

        let result =
            self.finish_fresh_worktree_root(&reservation, &expected_parent, registration_admission);
        match result {
            Ok(root_identity_json) => {
                let completion = verify_private_worktree_root_permissions(
                    &reservation.worktree_root,
                )
                .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
                    path: reservation.worktree_root.clone(),
                    reason: format!(
                        "cannot verify private worktree-root permissions before binding: {reason}"
                    ),
                })
                .and_then(|()| {
                    let expected_root: BoundDirectoryIdentity =
                        serde_json::from_str(&root_identity_json).map_err(|error| {
                            BrokerOpError::WorktreeRootUnavailable {
                                path: reservation.worktree_root.clone(),
                                reason: format!(
                                    "created worktree-root identity is invalid: {error}"
                                ),
                            }
                        })?;
                    require_bound_directory_identity(&reservation.worktree_root, &expected_root)
                        .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
                            path: reservation.worktree_root.clone(),
                            reason: format!(
                                "created worktree-root identity changed before binding: {reason}"
                            ),
                        })?;
                    require_bound_directory_identity(
                        reservation.worktree_root.parent().ok_or_else(|| {
                            BrokerOpError::WorktreeRootUnavailable {
                                path: reservation.worktree_root.clone(),
                                reason: "reserved worktree root has no parent before binding"
                                    .into(),
                            }
                        })?,
                        &expected_parent,
                    )
                    .map_err(|reason| {
                        BrokerOpError::WorktreeRootUnavailable {
                            path: reservation.worktree_root.clone(),
                            reason: format!(
                                "worktree-root parent changed before binding: {reason}"
                            ),
                        }
                    })?;
                    Ok(())
                })
                .and_then(|()| {
                    self.store
                        .complete_fresh_repository_worktree_binding(
                            &identity,
                            &reservation_digest,
                            &root_identity_json,
                        )
                        .map(|_| ())
                        .map_err(BrokerOpError::from)
                })
                .and_then(|()| {
                    let expected_root: BoundDirectoryIdentity = serde_json::from_str(
                        &root_identity_json,
                    )
                    .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
                        path: reservation.worktree_root.clone(),
                        reason: format!(
                            "created worktree-root identity is invalid after binding: {error}"
                        ),
                    })?;
                    require_bound_directory_identity(&reservation.worktree_root, &expected_root)
                        .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
                            path: reservation.worktree_root.clone(),
                            reason: format!(
                                "created worktree-root identity changed after binding: {reason}"
                            ),
                        })?;
                    require_bound_directory_identity(
                        reservation.worktree_root.parent().ok_or_else(|| {
                            BrokerOpError::WorktreeRootUnavailable {
                                path: reservation.worktree_root.clone(),
                                reason: "reserved worktree root has no parent after binding".into(),
                            }
                        })?,
                        &expected_parent,
                    )
                    .map_err(|reason| {
                        BrokerOpError::WorktreeRootUnavailable {
                            path: reservation.worktree_root.clone(),
                            reason: format!("worktree-root parent changed after binding: {reason}"),
                        }
                    })?;
                    Ok(())
                });
                if let Err(error) = completion {
                    if let Err(recovery_error) =
                        self.store.mark_repository_worktree_recovery_required(
                            &identity.bootstrap_nonce,
                            &reservation_digest,
                        )
                    {
                        crate::warn_unrecorded(
                            "retain incomplete fresh worktree-root binding as recovery required",
                            Err::<(), _>(recovery_error),
                        );
                    }
                    return Err(error);
                }
                self.storage_context
                    .initialize_preparation_cache_root(&self.repo)?;
                self.validate_bound_repository_worktree_root()?
                    .ok_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                        path: reservation.worktree_root.clone(),
                        reason: "root binding completed but could not be reopened".into(),
                    })?;
                Ok(WorktreePlacement {
                    root: reservation.worktree_root,
                    source: reservation.source,
                    outside_repository: reservation.source
                        != WorktreeRootSource::RepositoryFallback,
                    fallback_reason,
                })
            }
            Err(error) => {
                if let Err(recovery_error) = self.store.mark_repository_worktree_recovery_required(
                    &identity.bootstrap_nonce,
                    &reservation_digest,
                ) {
                    crate::warn_unrecorded(
                        "retain failed fresh worktree-root reservation as recovery required",
                        Err::<(), _>(recovery_error),
                    );
                }
                Err(error)
            }
        }
    }

    fn admit_fresh_worktree_root_registration(
        &self,
        root: &Path,
    ) -> Result<crate::worktree_registry::RegistrationAdmission, BrokerOpError> {
        crate::worktree_registry::admit(&self.host_operation_database_path()?, root).map_err(
            |error| BrokerOpError::WorktreeRootUnavailable {
                path: root.to_path_buf(),
                reason: format!("cannot retain worktree storage discovery: {error}"),
            },
        )
    }

    fn finish_fresh_worktree_root(
        &mut self,
        reservation: &crate::store::repository_binding::FreshRepositoryRootReservation,
        expected_parent: &BoundDirectoryIdentity,
        admission: crate::worktree_registry::RegistrationAdmission,
    ) -> Result<String, BrokerOpError> {
        self.finish_fresh_worktree_root_with_optional_admission_and_checkpoint_and_sync(
            reservation,
            expected_parent,
            Some(admission),
            |_, _| Ok(()),
            sync_bound_directory,
        )
    }

    #[cfg(all(test, unix))]
    fn finish_fresh_worktree_root_with_checkpoint(
        &mut self,
        reservation: &crate::store::repository_binding::FreshRepositoryRootReservation,
        expected_parent: &BoundDirectoryIdentity,
        checkpoint: impl FnMut(FreshRootPublicationCheckpoint, &Path) -> Result<(), String>,
    ) -> Result<String, BrokerOpError> {
        self.finish_fresh_worktree_root_with_checkpoint_and_sync(
            reservation,
            expected_parent,
            checkpoint,
            sync_bound_directory,
        )
    }

    #[cfg(all(test, unix))]
    fn finish_fresh_worktree_root_with_checkpoint_and_sync(
        &mut self,
        reservation: &crate::store::repository_binding::FreshRepositoryRootReservation,
        expected_parent: &BoundDirectoryIdentity,
        checkpoint: impl FnMut(FreshRootPublicationCheckpoint, &Path) -> Result<(), String>,
        sync_directory: impl FnMut(&Path, &BoundDirectoryIdentity) -> Result<(), String>,
    ) -> Result<String, BrokerOpError> {
        self.finish_fresh_worktree_root_with_optional_admission_and_checkpoint_and_sync(
            reservation,
            expected_parent,
            None,
            checkpoint,
            sync_directory,
        )
    }

    fn finish_fresh_worktree_root_with_optional_admission_and_checkpoint_and_sync(
        &mut self,
        reservation: &crate::store::repository_binding::FreshRepositoryRootReservation,
        expected_parent: &BoundDirectoryIdentity,
        admission: Option<crate::worktree_registry::RegistrationAdmission>,
        mut checkpoint: impl FnMut(FreshRootPublicationCheckpoint, &Path) -> Result<(), String>,
        mut sync_directory: impl FnMut(&Path, &BoundDirectoryIdentity) -> Result<(), String>,
    ) -> Result<String, BrokerOpError> {
        let root = &reservation.worktree_root;
        let parent = root
            .parent()
            .ok_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: "reserved root has no parent".into(),
            })?;
        if bound_directory_identity(parent).as_ref() != Ok(expected_parent) {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: parent.to_path_buf(),
                reason: "root parent identity changed after reservation".into(),
            });
        }
        let admission = match admission {
            Some(admission) => admission,
            None => self.admit_fresh_worktree_root_registration(root)?,
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            builder
                .create(root)
                .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
                    path: root.clone(),
                    reason: format!("cannot create reserved root without replacement: {error}"),
                })?;
        }
        #[cfg(not(unix))]
        {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: "fresh root creation requires Unix no-follow identity checks".into(),
            });
        }
        let identity = bound_directory_identity(root).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!("cannot capture created worktree-root identity: {reason}"),
            }
        })?;
        if !matches!(root.canonicalize(), Ok(actual) if actual == *root) {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: "created root did not retain its reserved canonical path".into(),
            });
        }
        self.worktree_root_marker_exists_or_empty(root)?;
        crate::host_state::protect_host_state_path(root, true).map_err(|error| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!("cannot apply private directory permissions: {error}"),
            }
        })?;
        verify_private_worktree_root_permissions(root).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!("cannot verify private worktree-root permissions: {reason}"),
            }
        })?;
        require_bound_directory_identity(root, &identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!(
                    "created worktree-root identity changed during protection: {reason}"
                ),
            }
        })?;
        sync_directory(parent, expected_parent).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: parent.to_path_buf(),
                reason: format!("cannot durably publish reserved worktree-root name: {reason}"),
            }
        })?;
        require_bound_directory_identity(root, &identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!(
                    "created worktree-root identity changed during admission: {reason}"
                ),
            }
        })?;
        self.write_or_verify_worktree_root_marker(root)?;
        require_bound_directory_identity(root, &identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!(
                    "created worktree-root identity changed after marker publication: {reason}"
                ),
            }
        })?;
        sync_directory(root, &identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!("cannot durably sync worktree-root marker namespace: {reason}"),
            }
        })?;
        checkpoint(
            FreshRootPublicationCheckpoint::MarkerDurablyPublishedBeforeRegistryRecord,
            root,
        )
        .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
            path: root.clone(),
            reason: format!("fresh-root publication stopped before registry record: {reason}"),
        })?;
        require_bound_directory_identity(root, &identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!(
                    "created worktree-root identity changed before registry record: {reason}"
                ),
            }
        })?;
        let marker = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: reservation.identity.repository_key.clone(),
            repository_root: reservation.identity.repository_root.clone(),
        };
        admission
            .record(root, &marker, reservation.source)
            .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!("cannot retain worktree storage discovery: {error}"),
            })?;
        require_bound_directory_identity(root, &identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!(
                    "created worktree-root identity changed after registry record: {reason}"
                ),
            }
        })?;
        checkpoint(
            FreshRootPublicationCheckpoint::RegistryRowDurablyRecorded,
            root,
        )
        .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
            path: root.clone(),
            reason: format!("fresh-root publication stopped after registry record: {reason}"),
        })?;
        require_bound_directory_identity(root, &identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!(
                    "created worktree-root identity changed after registry record checkpoint: {reason}"
                ),
            }
        })?;
        require_bound_directory_identity(parent, expected_parent).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: parent.to_path_buf(),
                reason: format!(
                    "worktree-root parent changed after registry record checkpoint: {reason}"
                ),
            }
        })?;
        crate::warn_unrecorded(
            "write worktree build defaults",
            write_worktree_build_defaults(root),
        );
        require_bound_directory_identity(root, &identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!(
                    "created worktree-root identity changed after build defaults: {reason}"
                ),
            }
        })?;
        sync_directory(root, &identity).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!("cannot durably sync created worktree-root entries: {reason}"),
            }
        })?;
        sync_directory(parent, expected_parent).map_err(|reason| {
            BrokerOpError::WorktreeRootUnavailable {
                path: parent.to_path_buf(),
                reason: format!("cannot durably sync reserved worktree-root parent: {reason}"),
            }
        })?;
        serde_json::to_string(&identity).map_err(|error| BrokerOpError::WorktreeRootUnavailable {
            path: root.clone(),
            reason: format!("cannot persist created worktree-root identity: {error}"),
        })
    }

    fn configured_worktree_minimum(
        &self,
        source: WorktreeRootSource,
    ) -> Result<Option<u64>, BrokerOpError> {
        if source != WorktreeRootSource::RepositoryConfig {
            return Ok(None);
        }
        crate::worktree_location::WorktreeLocationConfig::load(&self.main_root)
            .map(|config| config.map(|settings| settings.min_free_bytes()))
            .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
                path: self.main_root.join(".aethyme/config.toml"),
                reason,
            })
    }

    fn preflight_worktree_checkout_count(&self) -> Result<(), BrokerOpError> {
        // Invalid placement configuration already has a documented host-root
        // fallback and diagnostic. Keep that behavior, with the bounded
        // default rather than letting invalid policy disable admission.
        let config = match crate::worktree_location::WorktreeLocationConfig::load(&self.main_root) {
            Ok(Some(config)) => config,
            Ok(None) | Err(_) => crate::worktree_location::WorktreeLocationConfig::default(),
        };
        let maximum = config.max_checkouts();
        if maximum == 0 {
            return Ok(());
        }
        // Git's strict inventory includes its primary checkout. Count every
        // other registration, including prunable ones and other placement
        // roots, without opening or recursively sizing any checkout.
        let count = self.repo.worktree_inventory()?.len().saturating_sub(1);
        if count >= maximum as usize {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: self.main_root.clone(),
                reason: format!(
                    "linked checkout limit reached ({count}/{maximum}); no checkout was allocated. Reuse an existing worktree with broker start --reuse <worktree>, finish delivered sessions, or inspect broker gc plan. Configure [worktrees].max_checkouts to change the limit; explicit zero disables it"
                ),
            });
        }
        Ok(())
    }

    fn preflight_session_checkout_storage(
        &self,
        placement: &WorktreePlacement,
    ) -> Result<(), BrokerOpError> {
        let configured_minimum = self.configured_worktree_minimum(placement.source)?;
        preflight_worktree_root(
            &self.main_root,
            &placement.root,
            placement.source,
            placement.source != WorktreeRootSource::RepositoryFallback,
            configured_minimum,
            crate::verification::removable_volume_unavailable_reason,
            |candidate| {
                crate::disk_headroom::available_headroom_at_or_above_for(&self.main_root, candidate)
                    .map(|headroom| headroom.bytes)
            },
        )
        .map_err(|reason| BrokerOpError::WorktreeRootUnavailable {
            path: placement.root.clone(),
            reason,
        })
    }

    fn worktree_root_marker_exists_or_empty(&self, root: &Path) -> Result<bool, BrokerOpError> {
        let marker_path = root.join(WORKTREE_ROOT_MARKER);
        match std::fs::symlink_metadata(&marker_path) {
            Ok(_) => {
                self.verify_worktree_root_marker(
                    &marker_path,
                    &WorktreeRootMarker {
                        schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
                        repository_key: self.repository_worktree_key()?,
                        repository_root: self.main_root.clone(),
                    },
                )?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let first = std::fs::read_dir(root)
                    .and_then(|mut entries| entries.next().transpose())
                    .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
                        path: root.to_path_buf(),
                        reason: format!("cannot inspect unmarked worktree root: {error}"),
                    })?;
                if first.is_some() {
                    return Err(BrokerOpError::WorktreeRootUnavailable {
                        path: root.to_path_buf(),
                        reason: "nonempty unmarked directory cannot become a broker-owned root"
                            .into(),
                    });
                }
                Ok(false)
            }
            Err(error) => Err(BrokerOpError::WorktreeRootUnavailable {
                path: marker_path,
                reason: format!("cannot inspect ownership marker: {error}"),
            }),
        }
    }

    pub(super) fn write_or_verify_worktree_root_marker(
        &self,
        root: &Path,
    ) -> Result<(), BrokerOpError> {
        // Recheck after permission setup. An existing witness is never replaced.
        if self.worktree_root_marker_exists_or_empty(root)? {
            return Ok(());
        }
        let marker_path = root.join(WORKTREE_ROOT_MARKER);
        let expected = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: self.repository_worktree_key()?,
            repository_root: self.main_root.clone(),
        };
        let mut bytes = serde_json::to_vec_pretty(&expected).map_err(|error| {
            BrokerOpError::WorktreeRootUnavailable {
                path: marker_path.clone(),
                reason: error.to_string(),
            }
        })?;
        bytes.push(b'\n');
        if bytes.len() > crate::worktree_registry::MAX_RECORD_BYTES {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: marker_path,
                reason: "ownership marker exceeds its byte limit".into(),
            });
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&marker_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return self.verify_worktree_root_marker(&marker_path, &expected);
            }
            Err(error) => {
                return Err(BrokerOpError::WorktreeRootUnavailable {
                    path: marker_path,
                    reason: error.to_string(),
                });
            }
        };
        crate::host_state::protect_host_state_path(&marker_path, false).map_err(|error| {
            BrokerOpError::WorktreeRootUnavailable {
                path: marker_path.clone(),
                reason: format!("cannot apply private marker permissions: {error}"),
            }
        })?;
        // A crash can leave an invalid, bounded marker. Future admission refuses
        // it; it must not be repaired by silently adopting directory contents.
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
                path: marker_path.clone(),
                reason: error.to_string(),
            })?;
        self.verify_worktree_root_marker(&marker_path, &expected)
    }

    pub(super) fn verify_worktree_root_marker(
        &self,
        marker_path: &Path,
        expected: &WorktreeRootMarker,
    ) -> Result<(), BrokerOpError> {
        let root = marker_path
            .parent()
            .ok_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                path: marker_path.to_path_buf(),
                reason: "ownership marker has no root directory".into(),
            })?;
        let actual = crate::worktree_registry::read_marker(root).map_err(|error| {
            BrokerOpError::WorktreeRootUnavailable {
                path: marker_path.to_path_buf(),
                reason: format!("invalid ownership marker: {error}"),
            }
        })?;
        if &actual != expected {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: marker_path.to_path_buf(),
                reason: "ownership marker belongs to a different repository checkout".into(),
            });
        }
        Ok(())
    }

    pub(super) fn repository_worktree_key(&self) -> Result<String, BrokerOpError> {
        Ok(crate::host_state::repository_key(
            &self.main_root,
            Some(&self.main_git_common_dir()?),
        ))
    }

    /// [`GitRepo::git_common_dir`] of the main repository, cached after the
    /// first successful read; it cannot change while this broker is open.
    pub(super) fn main_git_common_dir(&self) -> Result<PathBuf, GitError> {
        if let Some(dir) = self.main_common_dir.get() {
            return Ok(dir.clone());
        }
        let dir = self.repo.git_common_dir()?;
        Ok(self.main_common_dir.get_or_init(|| dir).clone())
    }

    pub(super) fn path_is_inside_repository(&self, path: &Path) -> bool {
        root_path_is_inside_repository(&self.main_root, path).unwrap_or(true)
    }

    pub(super) fn refuse_nested_worktree_path(&self, path: &Path) -> Result<(), BrokerOpError> {
        for owner in self.repo.worktree_paths()? {
            let owner = owner.canonicalize().unwrap_or(owner);
            if path.starts_with(&owner) {
                return Err(BrokerOpError::NestedWorktreePath {
                    path: path.to_path_buf(),
                    owner,
                });
            }
        }
        Ok(())
    }

    /// A `status` row when a promoting integration branch no longer contains
    /// the fetched default branch's tip. New sessions start from upstream in
    /// that state (see [`Self::select_session_start_base_with_promote`]), but promotion and
    /// submit still target integration, so it needs reconciling. Silent in
    /// verify-only repositories, where integration is not used as a base.
    pub(super) fn integration_behind_upstream_advice(&self) -> Option<StatusAdvice> {
        let promote = PromoteConfig::load(&self.main_root);
        if promote.mode == crate::merge::PromoteMode::VerifyOnly {
            return None;
        }
        let integration_ref = format!("refs/heads/{}", promote.branch);
        let integration = self.repo.resolve_ref(&integration_ref)?;
        let (upstream_ref, upstream) = self.repo.upstream_default()?;
        if self.repo.is_ancestor(&upstream, &integration) {
            return None;
        }
        let (behind, ahead, _) = self.start_base_drift(&integration);
        let behind = behind.unwrap_or(0);
        let ahead = ahead.unwrap_or(0);
        Some(StatusAdvice {
            id: "integration.behind-upstream",
            severity: StatusAdviceSeverity::Warning,
            reason: "integration no longer contains the default branch",
            summary: format!(
                "{} is {behind} commit(s) behind {upstream_ref} and {ahead} ahead. New \
                 sessions start from {upstream_ref} instead; reconcile integration so \
                 submit and promotion stop targeting an old tree",
                promote.branch
            ),
            session_id: None,
            queue_entry_id: None,
            evidence: vec![
                format!("{} {}", promote.branch, short_commit(&integration)),
                format!("{upstream_ref} {}", short_commit(&upstream)),
            ],
            commands: vec![format!(
                "aethyme broker advanced integration reconcile --upstream {upstream_ref}"
            )],
        })
    }

    /// Compare a chosen start base against the fetched default branch.
    pub(super) fn start_base_drift(
        &self,
        commit: &str,
    ) -> (Option<u64>, Option<u64>, Option<String>) {
        let Some((upstream_ref, upstream_head)) = self.repo.upstream_default() else {
            return (None, None, None);
        };
        let behind = self.repo.commit_count_between(commit, &upstream_head).ok();
        let ahead = self.repo.commit_count_between(&upstream_head, commit).ok();
        (behind, ahead, Some(upstream_ref))
    }

    /// Choose the commit a new session's branch is cut from.
    ///
    /// An explicit `--base` skips inference entirely: the operator named the
    /// ref, so guessing on their behalf would be worse than failing. It is
    /// still measured against the default branch, because naming a base does
    /// not make its inherited commits stop landing in a pull request (#290).
    ///
    /// Without `--base`, integration is the base only when it is current: the
    /// promote mode promotes, and integration contains the fetched default
    /// branch's tip. Otherwise the session is cut from the fetched default
    /// branch, and the bypassed integration is reported. Choosing integration
    /// whenever it existed cut sessions 2,101 commits behind upstream in one
    /// repository, where it had stopped moving three days earlier. Nothing here
    /// fetches: `create_session_worktree` refreshes the default branch just
    /// before calling it (see `Broker::refresh_default_branch_before_start`),
    /// so the fetched tip it reads is current unless that refresh failed.
    fn select_session_start_base_with_promote(
        &self,
        explicit: Option<&str>,
        promote: &crate::merge::PromoteConfig,
    ) -> Result<SessionStartBase, BrokerOpError> {
        if let Some(reference) = explicit {
            let Some(commit) = self.repo.resolve_ref(reference) else {
                return Err(BrokerOpError::StartBaseUnavailable {
                    reason: format!(
                        "--base {reference} does not resolve to a commit in this repository"
                    ),
                });
            };
            let (behind_default_commits, ahead_default_commits, default_ref) =
                self.start_base_drift(&commit);
            return Ok(SessionStartBase {
                ref_name: reference.to_string(),
                commit,
                evidence: SessionStartBaseEvidence::ExplicitBase,
                behind_default_commits,
                ahead_default_commits,
                default_ref,
                bypassed_integration: None,
                fetched: None,
                fetch_error: None,
                cached_ref_age_seconds: None,
            });
        }
        let verify_only = promote.mode == crate::merge::PromoteMode::VerifyOnly;
        let integration_ref = format!("refs/heads/{}", promote.branch);
        let integration = self.repo.resolve_ref(&integration_ref);
        let upstream = self.repo.upstream_default();
        if let Some(commit) = &integration
            && !verify_only
            && upstream
                .as_ref()
                .is_none_or(|(_, tip)| self.repo.is_ancestor(tip, commit))
        {
            let (behind_default_commits, ahead_default_commits, default_ref) =
                self.start_base_drift(commit);
            return Ok(SessionStartBase {
                ref_name: integration_ref,
                commit: commit.clone(),
                evidence: SessionStartBaseEvidence::IntegrationTip,
                behind_default_commits,
                ahead_default_commits,
                default_ref,
                bypassed_integration: None,
                fetched: None,
                fetch_error: None,
                cached_ref_age_seconds: None,
            });
        }

        if let Some((upstream_ref, upstream_commit)) = upstream {
            let bypassed_integration = integration.map(|commit| {
                let (behind_default_commits, ahead_default_commits, _) =
                    self.start_base_drift(&commit);
                let behind = !verify_only;
                BypassedIntegration {
                    ref_name: integration_ref,
                    commit,
                    reason: if behind {
                        IntegrationBypassReason::BehindUpstream
                    } else {
                        IntegrationBypassReason::VerifyOnly
                    },
                    behind_default_commits,
                    ahead_default_commits,
                    recovery_command: behind.then(|| {
                        format!(
                            "aethyme broker advanced integration reconcile --upstream {upstream_ref}"
                        )
                    }),
                }
            });
            return Ok(SessionStartBase {
                ref_name: format!("refs/remotes/{upstream_ref}"),
                commit: upstream_commit,
                evidence: SessionStartBaseEvidence::FetchedDefaultBranch,
                behind_default_commits: Some(0),
                ahead_default_commits: Some(0),
                default_ref: Some(upstream_ref),
                bypassed_integration,
                fetched: None,
                fetch_error: None,
                cached_ref_age_seconds: None,
            });
        }
        // No fetched default branch: a promoting integration was taken above
        // (there is nothing to be behind), so only a verify-only repository or
        // one without integration reaches its local default branch here.
        if let Some(remote_head) = self.repo.symbolic_ref("refs/remotes/origin/HEAD")
            && let Some(branch_name) = remote_head.strip_prefix("refs/remotes/origin/")
        {
            let local_ref = format!("refs/heads/{branch_name}");
            if let Some(commit) = self.repo.resolve_ref(&local_ref) {
                return Ok(SessionStartBase {
                    ref_name: local_ref,
                    commit,
                    evidence: SessionStartBaseEvidence::RemoteDefaultBranch,
                    behind_default_commits: None,
                    ahead_default_commits: None,
                    default_ref: None,
                    bypassed_integration: None,
                    fetched: None,
                    fetch_error: None,
                    cached_ref_age_seconds: None,
                });
            }
        }

        let main = self.repo.resolve_ref("refs/heads/main");
        let master = self.repo.resolve_ref("refs/heads/master");
        match (main, master) {
            (Some(commit), None) => Ok(SessionStartBase {
                ref_name: "refs/heads/main".into(),
                commit,
                evidence: SessionStartBaseEvidence::ConventionalMain,
                behind_default_commits: None,
                ahead_default_commits: None,
                default_ref: None,
                bypassed_integration: None,
                fetched: None,
                fetch_error: None,
                cached_ref_age_seconds: None,
            }),
            (None, Some(commit)) => Ok(SessionStartBase {
                ref_name: "refs/heads/master".into(),
                commit,
                evidence: SessionStartBaseEvidence::ConventionalMaster,
                behind_default_commits: None,
                ahead_default_commits: None,
                default_ref: None,
                bypassed_integration: None,
                fetched: None,
                fetch_error: None,
                cached_ref_age_seconds: None,
            }),
            (Some(_), Some(_)) => Err(BrokerOpError::StartBaseUnavailable {
                reason: "both refs/heads/main and refs/heads/master exist, but origin/HEAD does not select one".into(),
            }),
            (None, None) => Err(BrokerOpError::StartBaseUnavailable {
                reason: "no integration tip, origin/HEAD-backed local branch, or unambiguous main/master ref exists".into(),
            }),
        }
    }

    pub(super) fn next_worktree_slug(&self, task: &str, worktree_root: &Path) -> String {
        let base = slugify(task);
        for attempt in 0..1000 {
            let slug = if attempt == 0 {
                base.clone()
            } else {
                format!("{base}-{}", attempt + 1)
            };
            let branch = format!("refs/heads/agent/{slug}");
            let worktree_path = worktree_root.join(&slug);
            if !worktree_path.exists() && self.repo.resolve_ref(&branch).is_none() {
                return slug;
            }
        }
        format!("{base}-{}", now_ms())
    }
}

#[derive(Debug)]
pub(super) struct GatedAgent {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    released: bool,
}

impl GatedAgent {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn release(&mut self) -> std::io::Result<()> {
        use std::io::Write as _;

        let stdin = self.stdin.as_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "start-agent gate input is already closed",
            )
        })?;
        let token = crate::agent_supervisor::GO;
        stdin.write_all(format!("{token}\n").as_bytes())?;
        self.stdin.take();
        self.released = true;
        Ok(())
    }

    pub(super) fn abort(&mut self) -> Result<(), String> {
        self.stdin.take();
        match self.wait_for_exit(std::time::Duration::from_millis(250)) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(_) => {}
        }
        let mut signal_errors = Vec::new();
        if let Err(error) = self.signal(false) {
            signal_errors.push(error);
        }
        match self.wait_for_exit(std::time::Duration::from_millis(500)) {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(error) => signal_errors.push(error),
        }
        if let Err(error) = self.signal(true) {
            signal_errors.push(error);
        }
        match self.wait_for_exit(std::time::Duration::from_secs(2)) {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!(
                "pid {} remains unreaped after closing the gate and sending termination signals{}",
                self.pid(),
                if signal_errors.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", signal_errors.join("; "))
                }
            )),
            Err(error) => Err(format!(
                "could not confirm pid {} exited after closing the gate and sending termination signals: {error}{}",
                self.pid(),
                if signal_errors.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", signal_errors.join("; "))
                }
            )),
        }
    }

    fn wait_for_exit(&mut self, allowance: std::time::Duration) -> Result<bool, String> {
        let deadline = std::time::Instant::now() + allowance;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return Ok(true),
                Ok(None) if std::time::Instant::now() >= deadline => return Ok(false),
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(error) => return Err(format!("waiting for pid {}: {error}", self.pid())),
            }
        }
    }

    fn signal(&mut self, force: bool) -> Result<(), String> {
        #[cfg(unix)]
        {
            let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
            // SAFETY: the child was spawned with process_group(0), so its
            // still-unreaped PID is the exact process-group id owned here.
            let result = unsafe { libc::killpg(self.pid() as i32, signal) };
            if result == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            Err(format!(
                "signalling owned process group {}: {error}",
                self.pid()
            ))
        }
        #[cfg(not(unix))]
        {
            let action = if force { "force-stopping" } else { "stopping" };
            self.child
                .kill()
                .map_err(|error| format!("{action} owned child {}: {error}", self.pid()))
        }
    }
}

impl Drop for GatedAgent {
    fn drop(&mut self) {
        if !self.released {
            crate::warn_unrecorded(
                "stop and reap an unreleased gated agent during drop",
                self.abort(),
            );
        }
        #[cfg(unix)]
        if self.released {
            // The Child handle is dropped immediately afterward and never
            // waited elsewhere. Until this sole waiter reaps it, the PID cannot
            // be recycled. No signals are issued by this best-effort reaper.
            let pid = self.child.id() as libc::pid_t;
            crate::warn_unrecorded(
                "start released agent reaper",
                std::thread::Builder::new()
                    .name("aethyme-agent-reaper".into())
                    .spawn(move || {
                        loop {
                            let mut status = 0;
                            // SAFETY: this thread is the sole waiter for this unreaped
                            // owned child; status points to initialized writable storage.
                            let result = unsafe { libc::waitpid(pid, &mut status, 0) };
                            if result == pid {
                                break;
                            }
                            let error = std::io::Error::last_os_error();
                            if result < 0 && error.kind() == std::io::ErrorKind::Interrupted {
                                continue;
                            }
                            if result < 0 && error.raw_os_error() != Some(libc::ECHILD) {
                                crate::warn_unrecorded("reap released agent", Err::<(), _>(error));
                            }
                            break;
                        }
                    })
                    .map(|_| ()),
            );
        }
    }
}

pub(super) fn spawn_blocked_agent(
    command: &str,
    worktree: &Path,
    log: File,
    log_err: File,
    broker: Option<&Broker>,
) -> Result<GatedAgent, BrokerOpError> {
    let executable = broker.and_then(|broker| broker.agent_supervisor_executable.as_deref());
    let (mut process, nonce) = crate::agent_supervisor::process(
        command, worktree, &log, executable,
    )
    .map_err(|source| BrokerOpError::Spawn {
        command: command.to_owned(),
        source,
    })?;
    drop(log_err);
    process
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        process.process_group(0);
    }
    if let Some(broker) = broker {
        broker.configure_storage_context_for_child(&mut process)?;
    }
    let child = process.spawn().map_err(|source| BrokerOpError::Spawn {
        command: command.to_string(),
        source,
    })?;
    let mut agent = GatedAgent {
        child,
        stdin: None,
        released: false,
    };
    agent.stdin = agent.child.stdin.take();
    if agent.stdin.is_none() {
        let error = BrokerOpError::Spawn {
            command: command.to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the spawned shell has no gate input",
            ),
        };
        return Err(match agent.abort() {
            Ok(()) => error,
            Err(reason) => BrokerOpError::StartAgentRollback {
                primary: Box::new(error),
                recovery: vec![format!(
                    "gated shell pid {} did not stop/reap: {reason}",
                    agent.pid()
                )],
            },
        });
    }
    let readiness = agent
        .child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("missing supervisor readiness pipe"))
        .and_then(|mut stdout| crate::agent_supervisor::await_ready(&mut stdout, &nonce));
    if let Err(source) = readiness {
        let error = BrokerOpError::Spawn {
            command: command.to_owned(),
            source,
        };
        return Err(match agent.abort() {
            Ok(()) => error,
            Err(reason) => BrokerOpError::StartAgentRollback {
                primary: Box::new(error),
                recovery: vec![reason],
            },
        });
    }
    Ok(agent)
}

pub(super) struct CreatedAgentLog {
    pub(super) path: PathBuf,
    identity: AgentLogIdentity,
}

#[derive(Clone, Copy)]
struct AgentLogIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl AgentLogIdentity {
    #[cfg(unix)]
    fn from_file(file: &File) -> std::io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;

        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "new agent log is not a regular file",
            ));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    #[cfg(not(unix))]
    fn from_file(_file: &File) -> std::io::Result<Self> {
        Ok(Self {})
    }

    fn matches_path_or_absent(self, path: &Path) -> std::io::Result<bool> {
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_file() {
            return Ok(false);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Ok(metadata.dev() == self.device && metadata.ino() == self.inode)
        }
        #[cfg(not(unix))]
        {
            match self {
                Self {} => Ok(false),
            }
        }
    }
}

pub(super) fn create_agent_log(
    log_dir: &Path,
    slug: &str,
) -> Result<(CreatedAgentLog, File), BrokerOpError> {
    std::fs::create_dir_all(log_dir).map_err(|source| {
        BrokerOpError::Store(BrokerError::Io {
            path: log_dir.to_path_buf(),
            source,
        })
    })?;
    for attempt in 0..1_000_u32 {
        let name = if attempt == 0 {
            format!("{slug}.log")
        } else {
            format!("{slug}-{}.log", attempt + 1)
        };
        let path = log_dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                let identity = AgentLogIdentity::from_file(&file).map_err(|source| {
                    let source = std::io::Error::new(
                        source.kind(),
                        format!(
                            "could not capture the newly created agent log identity; retained {}: {source}",
                            path.display()
                        ),
                    );
                    BrokerOpError::Store(BrokerError::Io {
                        path: path.clone(),
                        source,
                    })
                })?;
                return Ok((CreatedAgentLog { path, identity }, file));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(BrokerOpError::Store(BrokerError::Io { path, source })),
        }
    }
    let path = log_dir.join(format!("{slug}-1001.log"));
    Err(BrokerOpError::Store(BrokerError::Io {
        path,
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique agent log name",
        ),
    }))
}

fn start_agent_registration_matches(
    session: &Session,
    worktree_path: &str,
    branch: &str,
    base: &str,
    pid: u32,
    command: &str,
    log_path: &str,
) -> bool {
    session.worktree_path == worktree_path
        && session.branch == branch
        && session.status == SessionStatus::Active
        && session.origin == SessionOrigin::Spawned
        && session.pid == Some(pid as i64)
        && session.command.as_deref() == Some(command)
        && session.log_path.as_deref() == Some(log_path)
        && session.adoption_base.as_deref() == Some(base)
        && session.adopted_head.as_deref() == Some(base)
        && session.diff_base.as_deref() == Some(base)
}

fn start_agent_setup_failure(
    repo: &GitRepo,
    worktree_path: &Path,
    branch: &str,
    base: &str,
    created_log: Option<&CreatedAgentLog>,
    primary: BrokerOpError,
) -> BrokerOpError {
    let recovery = rollback_unregistered_start(repo, worktree_path, branch, base, created_log);
    if recovery.is_empty() {
        primary
    } else {
        BrokerOpError::StartAgentRollback {
            primary: Box::new(primary),
            recovery,
        }
    }
}

fn inspect_partial_worktree_creation(
    repo: &GitRepo,
    worktree_path: &Path,
    branch: &str,
) -> Vec<String> {
    let inventory = match repo.worktree_inventory() {
        Ok(inventory) => inventory,
        Err(error) => {
            return vec![format!(
                "worktree creation failed and its partial state could not be inspected: {error}; inspect path {} and branch {branch}",
                worktree_path.display()
            )];
        }
    };
    let registered = inventory
        .iter()
        .any(|entry| same_path(&entry.path, worktree_path));
    let branch_ref = format!("refs/heads/{branch}");
    let branch_exists = repo.resolve_ref(&branch_ref).is_some();
    if registered || worktree_path.exists() || branch_exists {
        vec![format!(
            "worktree creation left or may have left unregistered artifacts; retained path {} (registered={registered}, branch_ref_present={branch_exists}) and branch {branch}",
            worktree_path.display()
        )]
    } else {
        Vec::new()
    }
}

pub(super) fn rollback_unregistered_start(
    repo: &GitRepo,
    worktree_path: &Path,
    branch: &str,
    base: &str,
    created_log: Option<&CreatedAgentLog>,
) -> Vec<String> {
    let inventory = match repo.worktree_inventory() {
        Ok(inventory) => inventory,
        Err(error) => {
            return vec![format!(
                "could not verify the created worktree before rollback: {error}; retained worktree {} and branch {branch}",
                worktree_path.display()
            )];
        }
    };
    let Some(entry) = inventory
        .iter()
        .find(|entry| same_path(&entry.path, worktree_path))
    else {
        return vec![format!(
            "created worktree {} is no longer in Git's inventory; retained its path/branch and log for inspection",
            worktree_path.display()
        )];
    };
    let expected_branch = format!("refs/heads/{branch}");
    if entry.branch.as_deref() != Some(expected_branch.as_str())
        || entry.head.as_deref() != Some(base)
    {
        return vec![format!(
            "created worktree identity changed before rollback (path {}, branch {:?}, head {:?}); retained it and log for inspection",
            worktree_path.display(),
            entry.branch,
            entry.head
        )];
    }
    let worktree = match GitRepo::discover(worktree_path) {
        Ok(worktree) => worktree,
        Err(error) => {
            return vec![format!(
                "could not reopen the exact created worktree for rollback: {error}; retained {} and branch {branch}",
                worktree_path.display()
            )];
        }
    };
    match worktree.dirty_paths() {
        Ok(paths) if paths.is_empty() => {}
        Ok(paths) => {
            return vec![format!(
                "created worktree {} acquired changes before rollback ({paths:?}); retained it and log for inspection",
                worktree_path.display()
            )];
        }
        Err(error) => {
            return vec![format!(
                "could not prove created worktree {} is clean: {error}; retained it and log for inspection",
                worktree_path.display()
            )];
        }
    }
    if let Err(error) = repo.worktree_remove_without_orphan_fallback(worktree_path) {
        return vec![format!(
            "could not remove the exact clean worktree {}: {error}; retained branch {branch} and log for inspection",
            worktree_path.display()
        )];
    }
    if !path_is_explicitly_absent(worktree_path) {
        return vec![format!(
            "Git reported removal but could not prove path {} absent; retained branch {branch} and log for inspection",
            worktree_path.display()
        )];
    }
    if let Err(error) = repo.delete_branch_ref_checked(branch, base) {
        return vec![format!(
            "removed the exact clean worktree {}, but could not delete branch {branch} at expected base {base}: {error}",
            worktree_path.display()
        )];
    }
    if let Some(created_log) = created_log
        && let Err(reason) = remove_created_agent_log(created_log)
    {
        return vec![format!(
            "removed the unregistered worktree and branch but retained agent log {}: {reason}",
            created_log.path.display()
        )];
    }
    Vec::new()
}

pub(super) fn remove_created_agent_log(created_log: &CreatedAgentLog) -> Result<(), String> {
    match created_log
        .identity
        .matches_path_or_absent(&created_log.path)
    {
        Ok(true) => match std::fs::remove_file(&created_log.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("removal failed: {error}")),
        },
        Ok(false) => Err("the path no longer identifies the file created by this start attempt; it was preserved".into()),
        Err(error) => Err(format!("could not verify the created file identity: {error}; it was preserved")),
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left == right
}

fn path_is_explicitly_absent(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

#[cfg(all(test, unix))]
mod unbound_preparation_cache_open_tests {
    use crate::store::repository_binding::{
        RepositoryWorktreeBindingOrigin, RepositoryWorktreeBindingState,
    };
    use std::path::Path;
    use std::process::Command;

    fn git(repository: &Path, arguments: &[&str]) {
        let mut command = Command::new("git");
        command.arg("-C").arg(repository).args(arguments);
        for (name, _) in
            std::env::vars_os().filter(|(name, _)| name.to_string_lossy().starts_with("GIT_"))
        {
            command.env_remove(name);
        }
        let output = command.output().expect("run isolated Git fixture command");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository_fixture() -> tempfile::TempDir {
        let repository = tempfile::tempdir().unwrap();
        git(
            repository.path(),
            &["init", "-q", "-b", "main", "--template="],
        );
        git(
            repository.path(),
            &[
                "-c",
                "user.name=Aethyme Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-qm",
                "fixture",
            ],
        );
        repository
    }

    #[test]
    fn fresh_unbound_open_initializes_explicit_cache_without_binding_or_creating_worktree_root() {
        let repository = repository_fixture();
        let storage = crate::test_storage::TestStorage::new();

        let broker = storage.open(repository.path()).unwrap();
        let binding = broker.store_ref().repository_worktree_binding().unwrap();
        assert_eq!(binding.state, RepositoryWorktreeBindingState::FreshUnbound);
        assert_eq!(binding.origin, Some(RepositoryWorktreeBindingOrigin::Fresh));
        assert!(!storage.worktree_root().exists());
        assert!(!repository.path().join(".aethyme/worktrees").exists());
        assert!(
            broker
                .storage_context()
                .preparation_cache_root()
                .unwrap()
                .unwrap()
                .is_dir()
        );
        assert!(
            std::fs::read_dir(storage.root().join("preparation-cache"))
                .unwrap()
                .next()
                .is_some(),
            "fresh-unbound open must write initialization artifacts in the selected cache root"
        );
    }

    #[test]
    fn legacy_unbound_open_leaves_explicit_cache_uninitialized_and_creates_no_root() {
        let repository = repository_fixture();
        let database = repository.path().join(".aethyme/broker.db");
        std::fs::create_dir_all(database.parent().unwrap()).unwrap();
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch(include_str!("../../tests/fixtures/v028-schema52.sql"))
            .unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let cache_root = storage.root().join("preparation-cache");

        let broker = storage.open(repository.path()).unwrap();
        let binding = broker.store_ref().repository_worktree_binding().unwrap();
        assert_eq!(binding.state, RepositoryWorktreeBindingState::LegacyUnbound);
        assert_eq!(
            binding.origin,
            Some(RepositoryWorktreeBindingOrigin::Legacy)
        );
        assert!(
            std::fs::read_dir(&cache_root).unwrap().next().is_none(),
            "legacy-unbound open must leave the explicitly selected cache root untouched"
        );
        assert!(!storage.worktree_root().exists());
        assert!(!repository.path().join(".aethyme/worktrees").exists());
    }
}

#[cfg(test)]
mod explicit_path_absence_tests {
    use super::path_is_explicitly_absent;

    #[test]
    fn absence_requires_metadata_to_report_not_found() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        assert!(path_is_explicitly_absent(&missing));

        let present = root.path().join("present");
        std::fs::write(&present, b"present").unwrap();
        assert!(!path_is_explicitly_absent(&present));

        #[cfg(unix)]
        {
            let non_directory_ancestor = present.join("child");
            assert!(!path_is_explicitly_absent(&non_directory_ancestor));
        }
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_is_not_treated_as_absent() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let dangling = root.path().join("dangling");
        symlink(root.path().join("missing-target"), &dangling).unwrap();

        assert!(!path_is_explicitly_absent(&dangling));
    }
}

#[cfg(all(test, unix))]
mod worktree_root_digest_tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn non_utf8_paths_are_refused_by_both_binding_digest_encoders() {
        let non_utf8_root = PathBuf::from(OsString::from_vec(vec![
            b'/', b'r', b'e', b'p', b'o', b'/', 0xff,
        ]));
        let identity = crate::store::repository_binding::RepositoryRootIdentity {
            repository_root: non_utf8_root.clone(),
            git_common_dir: non_utf8_root.join(".git"),
            repository_key: "repo-key".to_owned(),
            broker_db_path: non_utf8_root.join(".aethyme/broker.db"),
            bootstrap_nonce: "a1".repeat(32),
        };

        let fresh_error = fresh_root_reservation_digest(
            &identity,
            &non_utf8_root.join(".aethyme/worktrees"),
            crate::store::repository_binding::RepositoryWorktreeRootLayout::KeyedContainer,
            WorktreeRootSource::RepositoryFallback,
            r#"{"device":1,"inode":2}"#,
        )
        .unwrap_err();
        assert!(matches!(
            fresh_error,
            BrokerOpError::WorktreeRootUnavailable { reason, .. }
                if reason.contains("cannot encode root reservation identity")
        ));

        let git_common_dir = non_utf8_root.join(".git");
        let broker_db_path = non_utf8_root.join(".aethyme/broker.db");
        let worktree_root = non_utf8_root.join(".aethyme/worktrees");
        let legacy_result =
            legacy_worktree_root_binding_digest(LegacyWorktreeRootBindingDigestInput {
                repository_root: &non_utf8_root,
                git_common_dir: &git_common_dir,
                repository_key: "repo-key",
                broker_db_path: &broker_db_path,
                worktree_root: &worktree_root,
                layout: "keyed_container",
                source: WorktreeRootSource::RepositoryFallback,
                parent_identity: r#"{"device":1,"inode":2}"#,
                worktree_identity: r#"{"device":1,"inode":3}"#,
            });
        assert!(legacy_result.is_err());
    }

    #[test]
    fn utf8_digest_encoders_preserve_legacy_canonical_bytes() {
        let identity = crate::store::repository_binding::RepositoryRootIdentity {
            repository_root: PathBuf::from("/repo"),
            git_common_dir: PathBuf::from("/repo/.git"),
            repository_key: "repo-key".to_owned(),
            broker_db_path: PathBuf::from("/repo/.aethyme/broker.db"),
            bootstrap_nonce: "a1".repeat(32),
        };
        let root = PathBuf::from("/repo/.aethyme/worktrees");
        let layout = crate::store::repository_binding::RepositoryWorktreeRootLayout::KeyedContainer;
        let source = WorktreeRootSource::RepositoryFallback;
        let parent_identity = r#"{"device":1,"inode":2}"#;
        let worktree_identity = r#"{"device":1,"inode":3}"#;

        let expected_fresh_bytes = serde_json::to_vec(&serde_json::json!({
            "repository_root": identity.repository_root,
            "git_common_dir": identity.git_common_dir,
            "repository_key": identity.repository_key,
            "broker_db_path": identity.broker_db_path,
            "bootstrap_nonce": identity.bootstrap_nonce,
            "worktree_root": root,
            "layout": format!("{layout:?}"),
            "source": source,
            "parent_identity": parent_identity,
        }))
        .unwrap();
        let fresh_digest =
            fresh_root_reservation_digest(&identity, &root, layout, source, parent_identity)
                .unwrap();
        assert_eq!(fresh_digest, hex_digest(&expected_fresh_bytes));

        let expected_legacy_bytes = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "repository_root": identity.repository_root,
            "git_common_dir": identity.git_common_dir,
            "repository_key": identity.repository_key,
            "broker_db_path": identity.broker_db_path,
            "worktree_root": root,
            "layout": "keyed_container",
            "source": source,
            "parent_identity": parent_identity,
            "worktree_identity": worktree_identity,
        }))
        .unwrap();
        let legacy_digest =
            legacy_worktree_root_binding_digest(LegacyWorktreeRootBindingDigestInput {
                repository_root: &identity.repository_root,
                git_common_dir: &identity.git_common_dir,
                repository_key: &identity.repository_key,
                broker_db_path: &identity.broker_db_path,
                worktree_root: &root,
                layout: "keyed_container",
                source,
                parent_identity,
                worktree_identity,
            })
            .unwrap();
        assert_eq!(legacy_digest, hex_digest(&expected_legacy_bytes));
    }
}

#[cfg(test)]
mod worktree_root_candidate_tests {
    use super::*;

    fn plan(source: WorktreeRootSource, unavailable: Option<&str>) -> WorktreeRootPlan {
        WorktreeRootPlan {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_root: PathBuf::from("/repo"),
            repository_key: "repo-key".into(),
            preferred_root: Some(PathBuf::from("/external/repo-key")),
            preferred_source: Some(source),
            root_container: None,
            legacy_fallback_root: PathBuf::from("/repo/.aethyme/worktrees"),
            preferred_outside_repository: true,
            host_state_fallback_root: Some(PathBuf::from("/host/worktrees/repo-key")),
            preferred_unavailable_reason: unavailable.map(str::to_string),
            storage_context_refusal: None,
        }
    }

    #[test]
    fn configured_and_host_state_roots_follow_the_session_fallback_order() {
        let configured = plan(WorktreeRootSource::RepositoryConfig, None);
        assert_eq!(
            worktree_root_candidates_for(&configured),
            vec![
                (
                    PathBuf::from("/external/repo-key"),
                    WorktreeRootSource::RepositoryConfig,
                ),
                (
                    PathBuf::from("/host/worktrees/repo-key"),
                    WorktreeRootSource::HostState,
                ),
                (
                    PathBuf::from("/repo/.aethyme/worktrees"),
                    WorktreeRootSource::RepositoryFallback,
                ),
            ]
        );

        let unavailable = plan(WorktreeRootSource::RepositoryConfig, Some("low space"));
        assert_eq!(
            worktree_root_candidates_for(&unavailable),
            vec![
                (
                    PathBuf::from("/host/worktrees/repo-key"),
                    WorktreeRootSource::HostState,
                ),
                (
                    PathBuf::from("/repo/.aethyme/worktrees"),
                    WorktreeRootSource::RepositoryFallback,
                ),
            ]
        );

        let host = plan(WorktreeRootSource::HostState, None);
        assert_eq!(
            worktree_root_candidates_for(&host),
            vec![
                (
                    PathBuf::from("/external/repo-key"),
                    WorktreeRootSource::HostState,
                ),
                (
                    PathBuf::from("/repo/.aethyme/worktrees"),
                    WorktreeRootSource::RepositoryFallback,
                ),
            ]
        );
    }

    #[test]
    fn an_unmounted_strict_override_fails_before_creating_its_path() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        std::fs::create_dir(&repository).unwrap();
        let missing = temp.path().join("Volumes/Gone/worktrees/repo-key");
        let plan = plan_worktree_root_for(&repository, None, Some(&missing));

        assert_eq!(
            worktree_root_candidates_for(&plan),
            vec![(missing.clone(), WorktreeRootSource::LibraryOverride)]
        );
        let result = prepare_worktree_root_directory(
            &repository,
            &missing,
            WorktreeRootSource::LibraryOverride,
            true,
            None,
            |_| Some("volume /Volumes/Gone is not mounted".into()),
            |_| None,
        );

        assert!(result.unwrap_err().contains("not mounted"));
        assert!(!missing.exists());
    }

    #[test]
    fn a_configured_root_below_its_existing_floor_is_not_created() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        let base = temp.path().join("external");
        std::fs::create_dir_all(&repository).unwrap();
        std::fs::create_dir_all(&base).unwrap();
        let missing = base.join("repo-key");

        let result = prepare_worktree_root_directory(
            &repository,
            &missing,
            WorktreeRootSource::RepositoryConfig,
            true,
            Some(1024),
            |_| None,
            |_| Some(512),
        );

        let reason = result.unwrap_err();
        assert!(reason.contains("worktrees.min_free_bytes"), "{reason}");
        assert!(!missing.exists());
    }

    #[test]
    fn a_non_directory_ancestor_is_refused_before_creating_the_missing_suffix() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        let blocker = temp.path().join("external/blocker");
        std::fs::create_dir_all(&repository).unwrap();
        std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
        std::fs::write(&blocker, "not a directory").unwrap();
        let missing = blocker.join("new/worktree-root");

        let result = prepare_worktree_root_directory(
            &repository,
            &missing,
            WorktreeRootSource::LibraryOverride,
            true,
            None,
            |_| None,
            |_| None,
        );

        assert!(result.unwrap_err().contains("is not a directory"));
        assert!(!blocker.join("new").exists());
        assert!(!missing.exists());
    }

    #[test]
    fn unresolved_parent_traversal_is_refused_before_creating_any_path() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("base/repo");
        std::fs::create_dir_all(&repository).unwrap();
        let missing = temp.path().join("missing");
        // If create_dir_all followed this path, it would create `missing`,
        // walk back to the temp root, then create `new-root` inside the repo.
        let path = temp.path().join("missing/../base/repo/new-root");

        let result = prepare_worktree_root_directory(
            &repository,
            &path,
            WorktreeRootSource::LibraryOverride,
            true,
            None,
            |_| None,
            |_| None,
        );

        let reason = result.unwrap_err();
        assert!(reason.contains("ambiguous path resolution"), "{reason}");
        assert!(!missing.exists());
        assert!(!repository.join("new-root").exists());
    }

    #[test]
    fn every_placement_keeps_a_recovery_reserve_before_creating_storage() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        std::fs::create_dir_all(&repository).unwrap();
        for (source, path, require_external) in [
            (
                WorktreeRootSource::RepositoryConfig,
                temp.path().join("configured-root"),
                true,
            ),
            (
                WorktreeRootSource::EnvironmentOverride,
                temp.path().join("env-root"),
                true,
            ),
            (
                WorktreeRootSource::LibraryOverride,
                temp.path().join("library-root"),
                true,
            ),
            (
                WorktreeRootSource::HostState,
                temp.path().join("host-root"),
                true,
            ),
            (
                WorktreeRootSource::RepositoryFallback,
                repository.join(".aethyme/worktrees"),
                false,
            ),
        ] {
            let minimum = if source == WorktreeRootSource::RepositoryConfig {
                1024
            } else {
                crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES
            };
            let result = prepare_worktree_root_directory(
                &repository,
                &path,
                source,
                require_external,
                Some(1024),
                |_| None,
                |_| Some(minimum - 1),
            );
            assert!(result.is_err(), "{source:?} ignored its reserve");
            assert!(!path.exists(), "{source:?} wrote before admission");
            let admitted = prepare_worktree_root_directory(
                &repository,
                &path,
                source,
                require_external,
                Some(1024),
                |_| None,
                |_| Some(minimum),
            )
            .unwrap();
            assert_eq!(admitted, path.canonicalize().unwrap());
        }
    }

    #[test]
    fn unknown_volume_headroom_cannot_authorize_new_checkout_storage() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repo");
        std::fs::create_dir_all(&repository).unwrap();
        let root = temp.path().join("unknown-root");
        let result = prepare_worktree_root_directory(
            &repository,
            &root,
            WorktreeRootSource::LibraryOverride,
            true,
            None,
            |_| None,
            |_| None,
        );
        assert!(result.unwrap_err().contains("cannot inspect free space"));
        assert!(!root.exists());
    }

    #[test]
    fn a_configured_floor_cannot_disable_an_overrides_default_reserve() {
        assert_eq!(
            worktree_storage_minimum(WorktreeRootSource::RepositoryConfig, Some(0)),
            0
        );
        for source in [
            WorktreeRootSource::EnvironmentOverride,
            WorktreeRootSource::LibraryOverride,
            WorktreeRootSource::HostState,
            WorktreeRootSource::RepositoryFallback,
        ] {
            assert_eq!(
                worktree_storage_minimum(source, Some(0)),
                crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES
            );
        }
    }

    #[test]
    fn explicit_roots_never_receive_an_implicit_fallback() {
        for source in [
            WorktreeRootSource::EnvironmentOverride,
            WorktreeRootSource::LibraryOverride,
        ] {
            assert_eq!(
                worktree_root_candidates_for(&plan(source, None)),
                vec![(PathBuf::from("/external/repo-key"), source)]
            );
        }
    }
}

#[cfg(test)]
mod artifact_sweep_namespace_tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn worktree_volume_cache_capture_requires_owned_real_marker_bound_roots() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let placement_root = temporary.path().join("external/repository-key");
        std::fs::create_dir_all(&placement_root).unwrap();
        let placement_root = placement_root.canonicalize().unwrap();
        let repository_root = temporary.path().join("repository");
        std::fs::create_dir(&repository_root).unwrap();
        let marker = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: "repository-key".into(),
            repository_root: repository_root.canonicalize().unwrap(),
        };
        std::fs::write(
            placement_root.join(WORKTREE_ROOT_MARKER),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        let host_state = temporary.path().join("host-state");
        std::fs::create_dir(&host_state).unwrap();
        let resource_db = host_state.join("host-resources.db");
        std::fs::File::create(&resource_db).unwrap();
        let resource_db = resource_db.canonicalize().unwrap();

        assert!(
            capture_worktree_volume_preparation_cache_root(&placement_root, &marker, &resource_db,)
                .unwrap()
                .is_none(),
            "an absent child is ordinary absence"
        );

        let cache_root = placement_root.join(WORKTREE_VOLUME_PREPARATION_CACHE_ROOT);
        std::fs::create_dir(&cache_root).unwrap();
        assert!(
            capture_worktree_volume_preparation_cache_root(&placement_root, &marker, &resource_db,)
                .is_err(),
            "an existing but uninitialized child is not adopted"
        );
        std::fs::remove_dir(&cache_root).unwrap();

        let placement_identity =
            crate::preparation::rotation_debt::DirectoryIdentity::capture(&placement_root)
                .unwrap()
                .encode()
                .unwrap();
        crate::preparation::rotation_debt::ensure_preparation_cache_root_child(
            &placement_root,
            WORKTREE_VOLUME_PREPARATION_CACHE_ROOT,
            &placement_identity,
        )
        .unwrap();
        let canonical = cache_root.canonicalize().unwrap();
        let cache_root_capture =
            crate::preparation::rotation_debt::capture_preparation_cache_root(&cache_root).unwrap();
        crate::preparation::rotation_debt::initialize_preparation_cache_root(
            &canonical,
            &cache_root_capture.identity,
            &cache_root_capture.directory,
            &resource_db,
            || Ok(false),
        )
        .unwrap();
        let binding =
            capture_worktree_volume_preparation_cache_root(&placement_root, &marker, &resource_db)
                .unwrap()
                .unwrap();
        binding.validate().unwrap();
        assert_eq!(
            binding
                .root_guard
                .as_ref()
                .unwrap()
                .placement_root
                .as_ref()
                .unwrap()
                .path,
            placement_root
        );

        let mut wrong_marker = marker.clone();
        wrong_marker.repository_key = "different-repository".into();
        assert!(
            capture_worktree_volume_preparation_cache_root(
                &placement_root,
                &wrong_marker,
                &resource_db,
            )
            .is_err()
        );

        let displaced = placement_root.join("displaced-cache");
        std::fs::rename(&cache_root, &displaced).unwrap();
        symlink(&displaced, &cache_root).unwrap();
        assert!(
            binding.validate().is_err(),
            "replacement by symlink is refused"
        );
        assert!(
            capture_worktree_volume_preparation_cache_root(&placement_root, &marker, &resource_db,)
                .is_err()
        );
    }

    #[test]
    fn namespace_matches_the_bounded_child_context_and_tracks_effective_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = temporary.path().join("repo");
        let host_state = temporary.path().join("state");
        let context = BrokerStorageContext {
            repository_root: Some(repository.clone()),
            git_common_dir: Some(repository.join(".git")),
            host_state_dir: Some(host_state.clone()),
            host_resource_db_path: Some(host_state.join("host-resources.db")),
            host_cache_dir: Some(temporary.path().join("cache")),
            host_operation_db_path: Some(host_state.join("operations.db")),
            worktree_root_override: Some(temporary.path().join("worktrees")),
            worktree_root_environment_override: None,
            collaboration_state_dir: Some(temporary.path().join("collaboration")),
            preparation_cache_root_override: None,
            preparation_cache_root_identity: None,
            preparation_cache_root_state: None,
            preparation_cache_root_handle: None,
            host_state_dir_is_explicit: true,
            capture_error: None,
        };

        let encoded = CapturedBrokerStorageContextV2::encode(&context).unwrap();
        let decoded = serde_json::from_str::<CapturedBrokerStorageContextV2>(&encoded)
            .unwrap()
            .into_context()
            .unwrap();
        assert_eq!(
            context.artifact_sweep_namespace().unwrap(),
            decoded.artifact_sweep_namespace().unwrap()
        );

        let mut different_cache = context.clone();
        different_cache.host_cache_dir = Some(temporary.path().join("other-cache"));
        assert_ne!(
            context.artifact_sweep_namespace().unwrap(),
            different_cache.artifact_sweep_namespace().unwrap()
        );
        assert!(
            BrokerStorageContext::default()
                .artifact_sweep_namespace()
                .is_err()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn separate_cache_root_requires_writable_init_and_uses_v4_namespace() {
        use std::os::unix::fs::MetadataExt as _;

        let temporary = tempfile::tempdir().unwrap();
        let temporary_root = temporary.path().canonicalize().unwrap();
        let root = temporary_root.as_path().join("separate-cache");
        std::fs::create_dir(&root).unwrap();
        let host_state_dir = temporary_root.as_path().join("host-state");
        std::fs::create_dir(&host_state_dir).unwrap();
        let host_resource_db_path = host_state_dir.join("host-resources.db");
        std::fs::File::create(&host_resource_db_path).unwrap();
        let host_resource_db_path = host_resource_db_path.canonicalize().unwrap();
        let (canonical, root_identity, state, handle) =
            capture_preparation_cache_root(&root).unwrap();
        let uninitialized = BrokerStorageContext {
            repository_root: Some(temporary_root.as_path().join("repo")),
            git_common_dir: Some(temporary_root.as_path().join("repo/.git")),
            host_state_dir: Some(host_state_dir.clone()),
            host_resource_db_path: Some(host_resource_db_path.clone()),
            preparation_cache_root_override: Some(canonical.clone()),
            preparation_cache_root_identity: Some(root_identity.clone()),
            preparation_cache_root_state: Some(state),
            preparation_cache_root_handle: Some(handle.clone()),
            ..BrokerStorageContext::default()
        };
        assert!(uninitialized.preparation_cache_root_binding().is_err());
        assert!(CapturedBrokerStorageContextV4::encode(&uninitialized).is_err());
        assert!(CapturedBrokerStorageContextV2::encode(&uninitialized).is_err());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);

        let initialized = crate::preparation::rotation_debt::initialize_preparation_cache_root(
            &canonical,
            &root_identity,
            &handle,
            &host_resource_db_path,
            || Ok(false),
        )
        .unwrap();
        let (canonical, root_identity, state, handle) =
            capture_preparation_cache_root(&root).unwrap();
        let mut marked = uninitialized.clone();
        marked.preparation_cache_root_override = Some(canonical.clone());
        marked.preparation_cache_root_identity = Some(root_identity);
        marked.preparation_cache_root_state = Some(state);
        marked.preparation_cache_root_handle = Some(handle);
        let binding = marked.preparation_cache_root_binding().unwrap().unwrap();
        assert_eq!(
            binding.instance_nonce.as_deref(),
            Some(initialized.nonce.as_str())
        );
        assert_eq!(
            binding.path,
            canonical.join(format!(".aethyme-preparation-cache-{}", initialized.nonce))
        );

        let encoded = CapturedBrokerStorageContextV4::encode(&marked).unwrap();
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            value.get("version").and_then(serde_json::Value::as_u64),
            Some(u64::from(BROKER_OPEN_STORAGE_CONTEXT_V4_VERSION))
        );
        let decoded = serde_json::from_str::<CapturedBrokerStorageContextV4>(&encoded)
            .unwrap()
            .into_context()
            .unwrap();
        let decoded_binding = decoded.preparation_cache_root_binding().unwrap().unwrap();
        assert_eq!(decoded_binding.path, binding.path);
        assert_eq!(decoded_binding.identity, binding.identity);
        assert_eq!(decoded_binding.instance_nonce, binding.instance_nonce);
        decoded_binding.validate().unwrap();
        let decoded_guard = decoded_binding.root_guard.as_ref().unwrap();
        let expected_guard = binding.root_guard.as_ref().unwrap();
        assert_eq!(decoded_guard.root_path, expected_guard.root_path);
        assert_eq!(decoded_guard.root_identity, expected_guard.root_identity);
        assert_eq!(
            decoded_guard.captured_resource_db_path,
            expected_guard.captured_resource_db_path
        );
        let decoded_handle_metadata = decoded_guard.root_handle.metadata().unwrap();
        let expected_handle_metadata = expected_guard.root_handle.metadata().unwrap();
        assert_eq!(
            (decoded_handle_metadata.dev(), decoded_handle_metadata.ino()),
            (
                expected_handle_metadata.dev(),
                expected_handle_metadata.ino()
            )
        );
        assert_eq!(
            decoded.artifact_sweep_namespace().unwrap(),
            marked.artifact_sweep_namespace().unwrap()
        );

        let other_state = temporary_root.as_path().join("other-host-state");
        std::fs::create_dir(&other_state).unwrap();
        let other_db = other_state.join("host-resources.db");
        std::fs::File::create(&other_db).unwrap();
        let other_db = other_db.canonicalize().unwrap();
        let cache_entry = binding.path.join("entry");
        std::fs::create_dir(&cache_entry).unwrap();
        let cache_sentinel = cache_entry.join("payload");
        std::fs::write(&cache_sentinel, b"retain this cache").unwrap();
        let marker_path = canonical.join(".aethyme-preparation-cache-root-v2");
        let marker_before = std::fs::read(&marker_path).unwrap();

        assert!(
            crate::preparation::rotation_debt::initialize_preparation_cache_root(
                &canonical,
                marked.preparation_cache_root_identity.as_deref().unwrap(),
                marked.preparation_cache_root_handle.as_deref().unwrap(),
                &other_db,
                || panic!("a mismatched coordinator must be rejected before debt lookup"),
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&marker_path).unwrap(), marker_before);
        assert_eq!(
            std::fs::read(&cache_sentinel).unwrap(),
            b"retain this cache"
        );

        let mut forged = value.clone();
        forged["preparation_cache_coordinator_db_path"] =
            serde_json::Value::from(other_db.to_string_lossy().into_owned());
        assert!(
            serde_json::from_value::<CapturedBrokerStorageContextV4>(forged)
                .unwrap()
                .into_context()
                .is_err(),
            "a V4 payload cannot rebind a cache root to another coordinator"
        );

        let host_state_alias = temporary_root.as_path().join("host-state-alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&host_state_dir, &host_state_alias).unwrap();
        let mut alias_context = marked.clone();
        alias_context.host_state_dir = Some(host_state_alias.clone());
        alias_context.host_resource_db_path = Some(host_state_alias.join("host-resources.db"));
        let alias_payload = CapturedBrokerStorageContextV4::encode(&alias_context).unwrap();
        let alias_decoded = serde_json::from_str::<CapturedBrokerStorageContextV4>(&alias_payload)
            .unwrap()
            .into_context()
            .unwrap();
        assert_eq!(
            alias_decoded
                .preparation_cache_root_binding()
                .unwrap()
                .unwrap()
                .path,
            binding.path,
            "canonical aliases of the bound coordinator DB retain the same cache binding"
        );

        let mut changed_marker: serde_json::Value = serde_json::from_slice(&marker_before).unwrap();
        changed_marker["coordinator_db_path"] =
            serde_json::Value::from(other_db.to_string_lossy().into_owned());
        std::fs::write(&marker_path, serde_json::to_vec(&changed_marker).unwrap()).unwrap();
        assert!(binding.validate().is_err());
        let sweep = crate::storage::sweep_preparation_cache_for_roots_with_binding(
            &temporary_root.as_path().join("repo"),
            &[temporary_root.as_path().join("worktrees")],
            &binding,
            &host_resource_db_path,
            std::time::Instant::now() + Duration::from_secs(1),
        );
        assert!(
            !sweep.complete,
            "marker changes make cache cleanup incomplete"
        );
        assert_eq!(
            std::fs::read(&cache_sentinel).unwrap(),
            b"retain this cache"
        );
        std::fs::write(&marker_path, &marker_before).unwrap();

        let mut legacy = marked.clone();
        legacy.preparation_cache_root_override = None;
        legacy.preparation_cache_root_identity = None;
        legacy.preparation_cache_root_state = None;
        legacy.preparation_cache_root_handle = None;
        assert_ne!(
            marked.artifact_sweep_namespace().unwrap(),
            legacy.artifact_sweep_namespace().unwrap()
        );

        let old_root = temporary_root.as_path().join("moved-cache");
        std::fs::rename(&canonical, &old_root).unwrap();
        std::fs::create_dir(&canonical).unwrap();
        assert!(marked.preparation_cache_root_binding().is_err());
        assert!(CapturedBrokerStorageContextV4::encode(&marked).is_err());
        assert!(
            old_root
                .join(format!(".aethyme-preparation-cache-{}", initialized.nonce))
                .is_dir()
        );
        assert_eq!(std::fs::read_dir(&canonical).unwrap().count(), 0);
    }

    #[test]
    fn default_artifact_context_keeps_the_legacy_v2_payload_and_digest() {
        let context = BrokerStorageContext {
            repository_root: Some(PathBuf::from("/repo")),
            git_common_dir: Some(PathBuf::from("/repo/.git")),
            host_state_dir: Some(PathBuf::from("/state")),
            host_resource_db_path: Some(PathBuf::from("/state/host-resources.db")),
            host_cache_dir: Some(PathBuf::from("/cache")),
            host_operation_db_path: Some(PathBuf::from("/state/operations.db")),
            worktree_root_override: Some(PathBuf::from("/worktrees")),
            worktree_root_environment_override: None,
            collaboration_state_dir: Some(PathBuf::from("/collaboration")),
            preparation_cache_root_override: None,
            preparation_cache_root_identity: None,
            preparation_cache_root_state: None,
            preparation_cache_root_handle: None,
            host_state_dir_is_explicit: true,
            capture_error: None,
        };
        assert_eq!(
            CapturedBrokerStorageContextV2::encode(&context).unwrap(),
            r#"{"version":2,"repository_root":"/repo","git_common_dir":"/repo/.git","host_state_dir":"/state","host_resource_db_path":"/state/host-resources.db","host_cache_dir":"/cache","host_operation_db_path":"/state/operations.db","worktree_root_override":"/worktrees","worktree_root_environment_override":null,"collaboration_state_dir":"/collaboration","host_state_dir_is_explicit":true}"#
        );
        assert_eq!(
            context.artifact_sweep_namespace().unwrap(),
            "9020ef2f465ead9b87ebab4d29e42afe260cc3cd0cf1f5f23ec38678b983acc6"
        );
    }

    #[test]
    fn separate_cache_root_rejects_relative_and_missing_paths() {
        let relative = PathBuf::from("relative-cache-root");
        let context = BrokerStorageContext::capture(
            BrokerOpenOptions::default().with_preparation_cache_root(relative),
        );
        assert!(context.ensure_valid().is_err());
        let temporary = tempfile::tempdir().unwrap();
        let missing = temporary.path().join("not-created");
        let context = BrokerStorageContext::capture(
            BrokerOpenOptions::default().with_preparation_cache_root(missing.clone()),
        );
        assert!(context.ensure_valid().is_err());
        assert!(
            !missing.exists(),
            "capture must not create the override root"
        );
    }
}

#[cfg(test)]
mod graph_write_fence_tests {
    #[test]
    fn mutable_open_enrolls_graph_fence_and_snapshot_does_not() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.com"],
            vec!["commit", "--allow-empty", "-qm", "init"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&root)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(&root).unwrap();
        let marker = root
            .join(".git")
            .join(aethyme_graph_storage::write_guard::BROKER_MANAGED_GRAPH_MARKER);
        assert!(marker.is_file());
        drop(broker);
        std::fs::remove_file(&marker).unwrap();
        let snapshot = super::Broker::open_snapshot(&root).unwrap();
        assert!(
            !marker.exists(),
            "read-only snapshot must not enroll a marker"
        );
        drop(snapshot);
        let _broker = storage.open(&root).unwrap();
        assert!(marker.is_file());
    }
}
