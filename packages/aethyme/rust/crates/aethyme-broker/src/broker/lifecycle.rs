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
const BROKER_OPEN_STORAGE_CONTEXT_LEGACY_VERSION: u32 = 2;
const BROKER_OPEN_STORAGE_CONTEXT_V4_VERSION: u32 = 4;
const BROKER_OPEN_STORAGE_CONTEXT_MAX_BYTES: usize = 32 * 1024;
const ARTIFACT_SWEEP_NAMESPACE_V2_DOMAIN: &[u8] = b"aethyme-artifact-sweep-context-v2\0";
const ARTIFACT_SWEEP_NAMESPACE_V4_DOMAIN: &[u8] = b"aethyme-artifact-sweep-context-v4\0";

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
    validate_captured_preparation_cache_root(
        &guard.root_path,
        &guard.root_identity,
        &guard.state,
        Some(&guard.captured_resource_db_path),
        &guard.root_handle,
    )
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

fn plan_worktree_root_with_context(
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

impl Broker {
    pub fn open(path_inside_repo: &Path) -> Result<Self, BrokerOpError> {
        Self::open_with_options(path_inside_repo, BrokerOpenOptions::default())
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
        })
    }

    /// Open only far enough to migrate compatible broker storage and backfill
    /// the immutable contract of live pre-v9 sessions. The CLI uses this for
    /// an explicitly requested session continuation; unlike normal open it
    /// does not reconcile prepared shared operations before compatibility has
    /// authorized the command itself.
    pub fn open_for_compatibility_backfill(path_inside_repo: &Path) -> Result<Self, BrokerOpError> {
        let mut storage_context = BrokerStorageContext::capture(BrokerOpenOptions::default());
        storage_context.ensure_valid()?;
        let here = GitRepo::discover(path_inside_repo)?;
        let main_root = here.main_root()?;
        let repo = GitRepo::discover(&main_root)?;
        storage_context.bind_repository(&repo)?;
        let store = BrokerStore::open_in_repo(&main_root)?;
        storage_context.initialize_preparation_cache_root(&repo)?;
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
        };
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
        storage_context.initialize_preparation_cache_root(&repo)?;
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
        };
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
        for session in self.store.live_sessions()? {
            if session.repository_contract.is_some() {
                continue;
            }
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
        let mut report = self.adopt_with_options_and_context_unrefreshed(
            worktree,
            task,
            options,
            agent_identity,
            context,
        )?;
        // Refresh the default branch and say how far this checkout drifted
        // while it sat unused -- the comparison `push` makes (#462).
        let (drift, note) = self.reused_session_drift(&report.session);
        report.default_branch = drift;
        report.default_branch_note = note;
        Ok(report)
    }

    pub(super) fn adopt_with_options_and_context_unrefreshed(
        &mut self,
        worktree: &Path,
        task: Option<&str>,
        options: AdoptOptions,
        agent_identity: Option<&str>,
        context: SessionContext,
    ) -> Result<AdoptReport, BrokerOpError> {
        let context = self.session_context(context);
        if options.sync_integration && options.mode != AdoptMode::Reuse {
            return Err(BrokerOpError::ReuseSyncRequiresReuse);
        }
        let checkout = GitRepo::discover(worktree)?;
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
        let integration_sync = if options.sync_integration {
            Some(self.synchronize_reuse_checkout(&checkout)?)
        } else {
            None
        };
        let diff_base = checkout.head_commit().ok();
        let repository_contract = self.capture_repository_contract(checkout.root(), false)?;
        let foreign_files = checkout.untracked_paths()?;
        let mut outcome = AdoptOutcome::Created;
        let mut replaced_session_id = None;

        if let Some(existing) = existing {
            match options.mode {
                AdoptMode::Reuse => {
                    // A live session's baseline is its durable ownership
                    // boundary. A plain reuse may update task text and
                    // liveness, but must never move that boundary across
                    // pending commits. Explicit fast-forward synchronization
                    // is the one safe refresh: it already proved the checkout
                    // has no unique work before moving it to integration.
                    let refreshed_base = integration_sync
                        .as_ref()
                        .map(|sync| sync.after_head.as_str());
                    let session = self.store.reuse_session_with_context_and_leases(
                        LeaseRefusalPolicy::verify_only_at(&self.main_root),
                        existing.id,
                        task,
                        refreshed_base,
                        agent_identity,
                        &context,
                        &planned_paths,
                    )?;
                    self.store
                        .set_session_foreign_files(session.id, &foreign_files)?;
                    let integration_drift =
                        Some(self.adopt_integration_drift(&checkout, session.id, true)?);
                    let planned_explicit_leases =
                        self.planned_explicit_leases(session.id, &planned_paths)?;
                    let preparation = self.preparation_status(session.id)?;
                    return Ok(AdoptReport {
                        session,
                        outcome: AdoptOutcome::Reused,
                        integration_drift,
                        renamed_targets: Vec::new(),
                        integration_sync,
                        planned_explicit_leases,
                        preparation,
                        default_branch: None,
                        default_branch_note: None,
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
        let session = if let Some(replaced_session_id) = replaced_session_id {
            self.store.replace_session_with_context_and_leases(
                LeaseRefusalPolicy::verify_only_at(&self.main_root),
                replaced_session_id,
                &new_session,
                &context,
                &planned_paths,
            )?
        } else {
            self.store.register_session_with_context_and_leases(
                LeaseRefusalPolicy::verify_only_at(&self.main_root),
                &new_session,
                &context,
                &planned_paths,
            )?
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

    pub(super) fn synchronize_reuse_checkout(
        &mut self,
        checkout: &GitRepo,
    ) -> Result<AdoptIntegrationSync, BrokerOpError> {
        let dirty_paths = checkout.dirty_paths()?;
        if !dirty_paths.is_empty() {
            return Err(BrokerOpError::ReuseSyncDirty { paths: dirty_paths });
        }

        let before_head = checkout.head_commit()?;
        let (integration_branch, integration_head) = self.integration_head()?;
        let outcome = if before_head == integration_head {
            AdoptIntegrationSyncOutcome::AlreadyCurrent
        } else {
            if !checkout.is_ancestor(&before_head, &integration_head) {
                let ahead = checkout.commit_count_between(&integration_head, &before_head)?;
                let behind = checkout.commit_count_between(&before_head, &integration_head)?;
                let relation = match (ahead, behind) {
                    (_, 0) => AdoptIntegrationRelation::Ahead,
                    _ => AdoptIntegrationRelation::Diverged,
                };
                return Err(BrokerOpError::ReuseSyncNotFastForward {
                    session_head: before_head,
                    integration_head,
                    relation: relation.as_str(),
                });
            }
            checkout.fast_forward_checkout(&integration_head)?;
            AdoptIntegrationSyncOutcome::FastForwarded
        };

        let after_head = checkout.head_commit()?;
        if after_head != integration_head {
            return Err(BrokerOpError::ReuseSyncVerification {
                expected: integration_head,
                actual: after_head,
            });
        }
        Ok(AdoptIntegrationSync {
            outcome,
            integration_branch,
            integration_head,
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

        let submission_plan = self.store.session(session_id).ok().and_then(|session| {
            self.build_submission_plan(&session, &session_head, &integration_head)
                .ok()
        });
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
        let context = self.session_context(context);
        let planned_paths = normalize_planned_paths(paths)?;
        self.ensure_planned_paths_available(&planned_paths, None)?;
        let (_slug, branch, start_base, worktree, worktree_placement) =
            self.create_session_worktree(task, explicit_base)?;
        let base = start_base.commit.clone();
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
        let session = match self.store.register_session_with_context_and_leases(
            LeaseRefusalPolicy::verify_only_at(&self.main_root),
            &new_session,
            &context,
            &planned_paths,
        ) {
            Ok(session) => session,
            Err(error) => {
                let worktree_path = worktree.root().to_path_buf();
                let _ = self.repo.worktree_remove(&worktree_path, true);
                let _ = self.repo.delete_branch_ref_checked(&branch, &base);
                return Err(error.into());
            }
        };
        self.store.set_session_foreign_files(session.id, &[])?;
        let planned_explicit_leases = self.planned_explicit_leases(session.id, &planned_paths)?;
        let preparation = self.preparation_status(session.id)?;
        Ok(StartReport {
            session,
            start_base,
            worktree_placement,
            planned_explicit_leases,
            preparation,
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
        let (slug, branch, start_base, worktree, worktree_placement) =
            self.create_session_worktree(task, explicit_base)?;
        let base = start_base.commit.clone();
        let worktree_path = worktree.root().to_path_buf();
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
        let registration = self.store.register_session_with_context_and_leases(
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

        Ok(StartAgentReport {
            session,
            start_base,
            worktree_placement,
        })
    }

    pub(super) fn create_session_worktree(
        &mut self,
        task: &str,
        explicit_base: Option<&str>,
    ) -> Result<(String, String, SessionStartBase, GitRepo, WorktreePlacement), BrokerOpError> {
        let placement = self.prepare_broker_worktree_root()?;
        let slug = self.next_worktree_slug(task, &placement.root);
        let worktree_path = placement.root.join(&slug);
        self.refuse_nested_worktree_path(&worktree_path)?;
        let branch = format!("agent/{slug}");
        let refresh = self.refresh_default_branch_before_start();
        // Verify-only sessions start from the fetched default branch either
        // way; advancing a disposable integration here keeps it from feeding
        // a stale copy of main to this session's submit and finish (#352).
        self.refresh_disposable_integration(crate::IntegrationRefreshTrigger::Start);
        let mut start_base = self.select_session_start_base(explicit_base)?;
        refresh.record(&mut start_base);
        // Refresh may wait on another process or a remote. Sample the actual
        // destination again immediately before allocating a Git checkout.
        self.preflight_session_checkout_storage(&placement)?;
        let worktree = match self
            .repo
            .worktree_add(&worktree_path, &branch, &start_base.commit)
        {
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
        Ok((slug, branch, start_base, worktree, placement))
    }

    pub fn worktree_root_plan(&self) -> Result<WorktreeRootPlan, BrokerOpError> {
        self.storage_context.ensure_valid()?;
        let git_common_dir = self.main_git_common_dir()?;
        let mut context = self.storage_context.clone();
        if let Some(root) = &self.worktree_root_override {
            context.worktree_root_override = Some(root.clone());
            context.worktree_root_environment_override = None;
        }
        Ok(plan_worktree_root_with_context(
            &self.main_root,
            Some(&git_common_dir),
            &context,
        ))
    }

    pub(super) fn prepare_broker_worktree_root(&self) -> Result<WorktreePlacement, BrokerOpError> {
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
            match self.prepare_worktree_root(
                &path,
                source,
                source != WorktreeRootSource::RepositoryFallback,
            ) {
                Ok(root) => {
                    return Ok(WorktreePlacement {
                        root,
                        source,
                        outside_repository: source != WorktreeRootSource::RepositoryFallback,
                        fallback_reason,
                    });
                }
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
                }
            }
        }
        Err(
            last_error.unwrap_or_else(|| BrokerOpError::WorktreeRootUnavailable {
                path: plan.legacy_fallback_root,
                reason: "no worktree-root candidates are available".into(),
            }),
        )
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

    pub(super) fn prepare_worktree_root(
        &self,
        path: &Path,
        source: WorktreeRootSource,
        require_external: bool,
    ) -> Result<PathBuf, BrokerOpError> {
        let min_free_bytes = self.configured_worktree_minimum(source)?;
        // Verify the write volume before creating even the registry's control
        // state. Hold its capacity exclusion through root creation/recording;
        // a full or unavailable history must not strand another external root.
        preflight_worktree_root(
            &self.main_root,
            path,
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
            path: path.to_path_buf(),
            reason,
        })?;
        let admission =
            crate::worktree_registry::admit(&self.host_operation_database_path()?, path).map_err(
                |error| BrokerOpError::WorktreeRootUnavailable {
                    path: path.to_path_buf(),
                    reason: format!("cannot retain worktree storage discovery: {error}"),
                },
            )?;
        let root = prepare_worktree_root_directory(
            &self.main_root,
            path,
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
            path: path.to_path_buf(),
            reason,
        })?;
        if require_external && self.path_is_inside_repository(&root) {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: root,
                reason: format!(
                    "{} must resolve outside repository {}; choose an external AETHYME_WORKTREE_ROOT",
                    source.as_str(),
                    self.main_root.display()
                ),
            });
        }
        // Selecting a pathname is not consent to adopt its existing contents.
        // Refuse before changing permissions on an unowned directory.
        self.worktree_root_marker_exists_or_empty(&root)?;
        crate::host_state::protect_host_state_path(&root, true).map_err(|error| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!("cannot apply private directory permissions: {error}"),
            }
        })?;
        self.write_or_verify_worktree_root_marker(&root)?;
        admission
            .record(
                &root,
                &WorktreeRootMarker {
                    schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
                    repository_key: self.repository_worktree_key()?,
                    repository_root: self.main_root.clone(),
                },
                source,
            )
            .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!("cannot retain worktree storage discovery: {error}"),
            })?;
        // Best effort on purpose: this is a disk-hygiene default, and failing
        // to start a session over one is a far worse outcome than building a
        // worktree the way cargo would have anyway.
        let _ = write_worktree_build_defaults(&root);
        Ok(root)
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
    /// that state (see [`Self::select_session_start_base`]), but promotion and
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
    pub(super) fn select_session_start_base(
        &self,
        explicit: Option<&str>,
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
        let promote = PromoteConfig::load(&self.main_root);
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
    if worktree_path.exists() {
        return vec![format!(
            "Git reported removal but path {} remains; retained branch {branch} and log for inspection",
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
        let root = temporary.path().join("separate-cache");
        std::fs::create_dir(&root).unwrap();
        let host_state_dir = temporary.path().join("host-state");
        std::fs::create_dir(&host_state_dir).unwrap();
        let host_resource_db_path = host_state_dir.join("host-resources.db");
        std::fs::File::create(&host_resource_db_path).unwrap();
        let host_resource_db_path = host_resource_db_path.canonicalize().unwrap();
        let (canonical, root_identity, state, handle) =
            capture_preparation_cache_root(&root).unwrap();
        let uninitialized = BrokerStorageContext {
            repository_root: Some(temporary.path().join("repo")),
            git_common_dir: Some(temporary.path().join("repo/.git")),
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

        let other_state = temporary.path().join("other-host-state");
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

        let host_state_alias = temporary.path().join("host-state-alias");
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
            &temporary.path().join("repo"),
            &[temporary.path().join("worktrees")],
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

        let old_root = temporary.path().join("moved-cache");
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
