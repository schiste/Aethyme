//! Lease-bound environment lifecycle (#1211).
//!
//! `up` reserves, provisions, starts and health-checks an environment;
//! `down` destroys it. Three rules carry the leak fix:
//!
//! 1. **Release only after destroy.** An environment's leases are released
//!    only once every component's destroy succeeded. A failed destroy leaves
//!    the environment `destroy_failed` with its leases held and the error
//!    visible, and generic `resources release` / `reconcile` refuse leases an
//!    environment owns (`environment_owned`). `env destroy --abandon --reason`
//!    is the one escape hatch, and it is recorded.
//! 2. **Destroy before the worktree goes.** Hook scripts usually live in the
//!    worktree, so destroy runs before any worktree removal: the broker's
//!    common removal point calls [`destroy_for_worktree`] and refuses to
//!    remove a worktree whose environments could not be destroyed.
//! 3. **Worktree already gone.** After a crash plus a manual `rm -rf`, the
//!    orphan sweep destroys from a temporary export of the recorded commit
//!    (`git archive`, no worktree registration); compose and container
//!    destroys act on recorded ids and need no files.
//!
//! Every external step is preceded by a durable phase (write-ahead), so a
//! crash leaves a record that says what may exist.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use super::compile::CompiledEnvironment;
use super::drivers::{ComponentIdentity, DriverContext, driver_for, wait_healthy};
use super::exec::{Bindings, DriverError, append_log};
use super::logs::remove_environment_directory;
use super::store::{EnvironmentPhase, EnvironmentRecord, EnvironmentStore, StoreError};
use crate::managed_services::ManagedServicePhase;
use sha2::{Digest, Sha256};

use crate::resources::{
    HOST_RESOURCE_REQUEST_SCHEMA_VERSION, HostResourceCoordinator, HostResourceError,
    HostResourceGrant, HostResourceKind, HostResourceRequest, HostResourceRequirement,
};

pub const RESERVATION_GRANT: &str = "reservation.grant.json";
pub const CAPACITY_GRANT: &str = "capacity.grant.json";
/// Destroy retries the supervisor makes before waiting for an operator.
pub const MAX_DESTROY_RETRIES: usize = 5;
/// Longest an [`Lifecycle::exclusive`] key is held before it lapses.
pub const EXCLUSIVE_KEY_TTL_SECONDS: u64 = 1800;

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("host resources: {0}")]
    Resources(#[from] HostResourceError),
    #[error("{0}")]
    Driver(#[from] DriverError),
    #[error("environment {env_id} is {phase}; {next}")]
    WrongPhase {
        env_id: String,
        phase: &'static str,
        next: String,
    },
    #[error(
        "environment {env_id} was brought up from a different profile snapshot; pass --reconfigure to destroy and recreate it"
    )]
    ReconfigureRequired { env_id: String },
    #[error("{0}")]
    Io(String),
}

impl LifecycleError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Store(StoreError::NotFound(_)) => "not_found",
            Self::Store(_) => "store_error",
            Self::Resources(HostResourceError::Conflict { .. }) => "capacity_timeout",
            Self::Resources(_) => "resource_error",
            Self::Driver(error) => error.code(),
            Self::WrongPhase { .. } => "wrong_phase",
            Self::ReconfigureRequired { .. } => "reconfigure_required",
            Self::Io(_) => "io_error",
        }
    }
}

/// Everything a lifecycle operation touches. Paths are injectable so tests
/// never use real host state.
pub struct Lifecycle {
    pub store: EnvironmentStore,
    pub coordinator: HostResourceCoordinator,
    /// `<host state>/environments`.
    pub root: PathBuf,
    /// Recorded as `holder_pid` on leases: the supervisor when one runs.
    pub holder_pid: Option<u32>,
    pub hook_timeout: Duration,
    pub actor: String,
}

pub struct UpOptions {
    pub wait: Duration,
    pub reconfigure: bool,
}

fn grant_path(root: &Path, env_id: &str, name: &str) -> PathBuf {
    root.join(env_id).join(name)
}

pub(super) fn write_grant(
    root: &Path,
    env_id: &str,
    name: &str,
    grant: &HostResourceGrant,
) -> Result<(), LifecycleError> {
    let directory = root.join(env_id);
    std::fs::create_dir_all(&directory).map_err(|e| LifecycleError::Io(e.to_string()))?;
    crate::host_state::protect_host_state_path(&directory, true)
        .map_err(|e| LifecycleError::Io(e.to_string()))?;
    let path = directory.join(name);
    let text = serde_json::to_vec(grant).map_err(|e| LifecycleError::Io(e.to_string()))?;
    std::fs::write(&path, text).map_err(|e| LifecycleError::Io(e.to_string()))?;
    crate::host_state::protect_host_state_path(&path, false)
        .map_err(|e| LifecycleError::Io(e.to_string()))?;
    Ok(())
}

fn remove_grant(root: &Path, env_id: &str, name: &str) {
    match std::fs::remove_file(grant_path(root, env_id, name)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            crate::warn_unrecorded(&format!("remove {name}"), Err::<(), _>(error))
        }
        _ => {}
    }
}

pub fn read_grant(root: &Path, env_id: &str, name: &str) -> Option<HostResourceGrant> {
    let text = std::fs::read(grant_path(root, env_id, name)).ok()?;
    serde_json::from_slice(&text).ok()
}

pub(super) fn bindings_from(
    grant: Option<&HostResourceGrant>,
    worktree: &Path,
    receipts: BTreeMap<String, Value>,
) -> Bindings {
    let mut bindings = Bindings {
        worktree: worktree.to_path_buf(),
        receipts,
        ..Bindings::default()
    };
    if let Some(grant) = grant {
        for allocation in &grant.lease.allocations {
            if allocation.key == super::compile::NAMESPACE_KEY {
                bindings.namespace = allocation.value.clone();
            } else if let Some(component) = allocation.key.strip_prefix("port_")
                && let Ok(port) = allocation.value.parse()
            {
                bindings.ports.insert(component.to_string(), port);
            }
        }
    }
    bindings
}

impl Lifecycle {
    /// Run `body` holding the exclusive host key `name`, released afterwards
    /// whatever `body` returns. `None` when another holder kept the key past
    /// `wait` (or the key could not be acquired), and `body` did not run.
    pub(super) fn exclusive<T>(
        &mut self,
        repository: &str,
        name: &str,
        wait: Duration,
        body: impl FnOnce(&mut Self) -> T,
    ) -> Option<T> {
        // Request ids are unique forever, and a repeated id is a replay of
        // the same request: two threads asking in one millisecond must not
        // share one, or both would be granted the key.
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let digest = format!("{:x}", Sha256::digest(name.as_bytes()));
        let request = HostResourceRequest {
            schema_version: HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: format!(
                "env-key:{}:{}:{}:{sequence}",
                &digest[..24],
                std::process::id(),
                now_ms()
            ),
            repository: repository.to_string(),
            worktree_fingerprint: "environment-key".into(),
            run_id: format!("env-key-{}", &digest[..12]),
            ttl_seconds: EXCLUSIVE_KEY_TTL_SECONDS,
            holder_pid: Some(std::process::id()),
            resources: vec![HostResourceRequirement {
                key: "key".into(),
                resource: HostResourceKind::ExclusiveKey {
                    name: name.to_string(),
                },
            }],
        };
        let grant = self
            .coordinator
            .acquire_with_wait(&request, wait, |_| {})
            .ok()?;
        let result = body(self);
        crate::warn_unrecorded(
            "release an environment key",
            self.coordinator.release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            ),
        );
        Some(result)
    }

    pub(super) fn operation_lock(
        &self,
        env_id: &str,
    ) -> Result<crate::file_lock::ExclusiveFileLock, LifecycleError> {
        crate::file_lock::try_acquire_environment_operation_lock(&self.root, env_id)
            .map_err(|error| LifecycleError::Io(error.to_string()))
    }

    pub(super) fn context<'a>(
        &self,
        environment: &'a CompiledEnvironment,
        component: &'a super::compile::CompiledComponent,
        bindings: &'a Bindings,
        attempt: u32,
    ) -> DriverContext<'a> {
        DriverContext {
            environment,
            component,
            bindings,
            attempt,
            directory: self.root.join(&environment.env_id),
            hook_timeout: self.hook_timeout,
        }
    }

    pub(super) fn receipts(&self, env_id: &str) -> Result<BTreeMap<String, Value>, LifecycleError> {
        Ok(self
            .store
            .components(env_id)?
            .into_iter()
            .filter_map(|c| c.receipt.map(|r| (c.component, r)))
            .collect())
    }

    pub(super) fn advance(
        &mut self,
        record: &EnvironmentRecord,
        to: EnvironmentPhase,
        reason: &str,
        detail: Option<Value>,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        Ok(self.store.transition(
            &record.env_id,
            record.phase_generation,
            to,
            reason,
            &self.actor,
            detail,
        )?)
    }

    pub(super) fn component_to(
        &mut self,
        env_id: &str,
        component: &str,
        phases: &[ManagedServicePhase],
        identity: Option<&Value>,
        receipt: Option<&Value>,
    ) -> Result<(), LifecycleError> {
        let current = self
            .store
            .components(env_id)?
            .into_iter()
            .find(|c| c.component == component)
            .map(|c| c.phase);
        // Resume after the current phase when it is on the path, so a
        // replay never steps a component backwards.
        let start = current
            .and_then(|phase| phases.iter().position(|p| *p == phase))
            .map_or(0, |index| index + 1);
        for (index, phase) in phases.iter().enumerate().skip(start) {
            let last = index + 1 == phases.len();
            self.store.set_component(
                env_id,
                component,
                *phase,
                if last { identity } else { None },
                if last { receipt } else { None },
            )?;
        }
        Ok(())
    }

    /// Bring an environment up from `snapshot`, or return it if already up.
    pub fn up(
        &mut self,
        snapshot: &CompiledEnvironment,
        worktree: &Path,
        main_root: Option<&Path>,
        session_id: Option<i64>,
        options: &UpOptions,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        let _operation = self.operation_lock(&snapshot.env_id)?;
        self.up_locked(snapshot, worktree, main_root, session_id, options)
    }

    fn up_locked(
        &mut self,
        snapshot: &CompiledEnvironment,
        worktree: &Path,
        main_root: Option<&Path>,
        session_id: Option<i64>,
        options: &UpOptions,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        let mut record = self.store.register(snapshot, Some(worktree), session_id)?;
        if let Some(main_root) = main_root {
            self.store.set_main_root(&record.env_id, main_root)?;
        }
        if record.profile_digest != snapshot.digest {
            if !options.reconfigure {
                return Err(LifecycleError::ReconfigureRequired {
                    env_id: record.env_id,
                });
            }
            self.down_locked(&record.env_id, None)?;
            record = self.store.register(snapshot, Some(worktree), session_id)?;
        }
        if record.phase == EnvironmentPhase::Failed
            && (record.capacity_lease_id.is_some()
                || self
                    .store
                    .components(&record.env_id)?
                    .iter()
                    .any(|component| component.identity.is_some()))
        {
            return Err(LifecycleError::WrongPhase {
                env_id: record.env_id,
                phase: "failed",
                next: "run `aethyme env destroy <id> --retry` to settle the previous attempt before retrying up".into(),
            });
        }
        match record.phase {
            EnvironmentPhase::Active => {
                self.store.record_activity(&record.env_id, now_ms())?;
                return Ok(record);
            }
            EnvironmentPhase::Planned | EnvironmentPhase::Failed => {}
            // `up` is "make sure it is up": a parked environment is woken.
            EnvironmentPhase::Parked => return self.wake_locked(&record.env_id, options.wait),
            other => {
                return Err(LifecycleError::WrongPhase {
                    env_id: record.env_id,
                    phase: other.as_str(),
                    next: match other {
                        EnvironmentPhase::DestroyFailed => {
                            "run `aethyme env destroy <id> --retry`".into()
                        }
                        _ => "wait for the current operation, or check `aethyme env status <id>`"
                            .into(),
                    },
                });
            }
        }
        self.bring_up(record, worktree, options)
    }

    fn bring_up(
        &mut self,
        record: EnvironmentRecord,
        worktree: &Path,
        options: &UpOptions,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        let snapshot = record.snapshot.clone();
        let env_id = record.env_id.clone();
        let instance = record.phase_generation as u64;
        // Reservation: namespace and ports, held until destroy is confirmed.
        let reservation = match read_grant(&self.root, &env_id, RESERVATION_GRANT) {
            Some(grant)
                if record.reservation_lease_id.as_deref()
                    == Some(grant.lease.lease_id.as_str()) =>
            {
                grant
            }
            _ => {
                let grant = self
                    .coordinator
                    .acquire(&snapshot.reservation_request(self.holder_pid, instance))?;
                write_grant(&self.root, &env_id, RESERVATION_GRANT, &grant)?;
                grant
            }
        };
        let capacity = match snapshot.capacity_request(self.holder_pid, instance) {
            Some(request) => {
                // A full `park_lru` pool parks idle victims first (#1215).
                self.make_room(&snapshot, now_ms())?;
                let grant = self
                    .coordinator
                    .acquire_with_wait(&request, options.wait, |_| {})?;
                write_grant(&self.root, &env_id, CAPACITY_GRANT, &grant)?;
                Some(grant)
            }
            None => None,
        };
        self.store.set_leases(
            &env_id,
            Some(&reservation.lease.lease_id),
            capacity.as_ref().map(|g| g.lease.lease_id.as_str()),
        )?;
        let bindings = bindings_from(Some(&reservation), worktree, self.receipts(&env_id)?);
        if record.namespace.is_none() && !bindings.namespace.is_empty() {
            self.store.set_namespace(&env_id, &bindings.namespace)?;
        }
        let record = self
            .store
            .get(&env_id)?
            .ok_or_else(|| StoreError::NotFound(env_id.clone()))?;
        self.advance(&record, EnvironmentPhase::Provisioning, "up", None)?;

        // Provision in dependency order. A crash between a provision and its
        // receipt is replayed with an incremented attempt; hooks must be
        // idempotent and can see the replay.
        let mut provisioned: Vec<String> = Vec::new();
        // Present (even empty) means prune can tell what this one uses.
        self.record_cache_uses(&env_id, Vec::new());
        let mut receipts = bindings.receipts.clone();
        for component in &snapshot.components {
            let previous = self
                .store
                .components(&env_id)?
                .into_iter()
                .find(|c| c.component == component.name);
            // Provisioned already: created, or started at some point since.
            if previous.as_ref().is_some_and(|c| {
                matches!(
                    c.phase,
                    ManagedServicePhase::Created
                        | ManagedServicePhase::StartPending
                        | ManagedServicePhase::Running
                        | ManagedServicePhase::RemovalPending
                        | ManagedServicePhase::Removed
                        | ManagedServicePhase::Settled
                )
            }) {
                provisioned.push(component.name.clone());
                continue;
            }
            let replay = previous
                .as_ref()
                .is_some_and(|c| c.phase == ManagedServicePhase::CreatePending);
            self.component_to(
                &env_id,
                &component.name,
                &[
                    ManagedServicePhase::AdmissionPending,
                    ManagedServicePhase::Admitted,
                    ManagedServicePhase::CreatePending,
                ],
                None,
                None,
            )?;
            let attempt = if replay { 2 } else { 1 };
            let mut current = Bindings {
                receipts: receipts.clone(),
                ..bindings.clone()
            };
            let driver = driver_for(component.runtime);
            // Template first (its id reaches provision), then the build from
            // the cache when its inputs are unchanged (#1219).
            let template = {
                let ctx = self.context(&snapshot, component, &current, attempt);
                self.ensure_template(&ctx)
            };
            let outcome = match template {
                Err(error) => Err(error),
                Ok(template) => {
                    if let Some(id) = template {
                        current.template_ids.insert(component.name.clone(), id);
                    }
                    let ctx = self.context(&snapshot, component, &current, attempt);
                    match self.cached_build(&ctx) {
                        Err(error) => Err(error),
                        Ok(()) => driver.provision(&ctx),
                    }
                }
            };
            match outcome {
                Ok(receipt) => {
                    if let Some(receipt) = &receipt {
                        receipts.insert(component.name.clone(), receipt.clone());
                    }
                    self.component_to(
                        &env_id,
                        &component.name,
                        &[ManagedServicePhase::Created],
                        None,
                        receipt.as_ref(),
                    )?;
                    provisioned.push(component.name.clone());
                }
                Err(error) => {
                    return self.fail_provision(
                        &env_id,
                        &snapshot,
                        worktree,
                        &provisioned,
                        &receipts,
                        error,
                    );
                }
            }
        }
        let record = self
            .store
            .get(&env_id)?
            .ok_or_else(|| StoreError::NotFound(env_id.clone()))?;
        let record = self.advance(&record, EnvironmentPhase::Starting, "start", None)?;
        let bindings = Bindings {
            receipts: receipts.clone(),
            ..bindings
        };
        let mut started: Vec<(String, ComponentIdentity)> = Vec::new();
        for component in &snapshot.components {
            self.prepare_start(&env_id, &snapshot, &bindings, component)?;
            let ctx = self.context(&snapshot, component, &bindings, 1);
            let driver = driver_for(component.runtime);
            let identity = match driver.start(&ctx) {
                Ok(identity) => identity,
                Err(error) => {
                    return self.fail_start(
                        &record,
                        &snapshot,
                        &bindings,
                        started,
                        &component.name,
                        error,
                    );
                }
            };
            let identity_json =
                serde_json::to_value(&identity).map_err(|e| LifecycleError::Io(e.to_string()))?;
            self.component_to(
                &env_id,
                &component.name,
                &[ManagedServicePhase::Running],
                Some(&identity_json),
                None,
            )?;
            started.push((component.name.clone(), identity.clone()));
            if let Err(error) = wait_healthy(driver.as_ref(), &ctx, &identity) {
                return self.fail_start(
                    &record,
                    &snapshot,
                    &bindings,
                    started,
                    &component.name,
                    error,
                );
            }
        }
        let record = self.advance(&record, EnvironmentPhase::Active, "healthy", None)?;
        self.store.set_error(&env_id, None)?;
        self.store.record_activity(&env_id, now_ms())?;
        Ok(self.store.get(&env_id)?.unwrap_or(record))
    }

    /// Bring a component's record to `start_pending` from wherever an earlier
    /// run left it. A component still recorded as running from a crashed run
    /// is stopped first (only if its identity is proven), then a new instance
    /// starts.
    pub(super) fn prepare_start(
        &mut self,
        env_id: &str,
        snapshot: &CompiledEnvironment,
        bindings: &Bindings,
        component: &super::compile::CompiledComponent,
    ) -> Result<(), LifecycleError> {
        let current = self
            .store
            .components(env_id)?
            .into_iter()
            .find(|c| c.component == component.name);
        let Some(current) = current else {
            return Err(StoreError::NotFound(format!("{env_id}/{}", component.name)).into());
        };
        use ManagedServicePhase::*;
        let path: &[ManagedServicePhase] = match current.phase {
            Created => &[StartPending],
            StartPending | Running => {
                if let Some(identity) = current
                    .identity
                    .clone()
                    .and_then(|v| serde_json::from_value::<ComponentIdentity>(v).ok())
                {
                    let ctx = self.context(snapshot, component, bindings, 1);
                    driver_for(component.runtime).stop(&ctx, &identity)?;
                }
                &[
                    RemovalPending,
                    Removed,
                    Settled,
                    Intent,
                    AdmissionPending,
                    Admitted,
                    CreatePending,
                    Created,
                    StartPending,
                ]
            }
            Settled => &[
                Intent,
                AdmissionPending,
                Admitted,
                CreatePending,
                Created,
                StartPending,
            ],
            RemovalPending | Removed => &[
                Removed,
                Settled,
                Intent,
                AdmissionPending,
                Admitted,
                CreatePending,
                Created,
                StartPending,
            ],
            Intent | AdmissionPending | Admitted | CreatePending => &[
                AdmissionPending,
                Admitted,
                CreatePending,
                Created,
                StartPending,
            ],
        };
        self.component_to(env_id, &component.name, path, None, None)
    }

    fn fail_provision(
        &mut self,
        env_id: &str,
        snapshot: &CompiledEnvironment,
        worktree: &Path,
        provisioned: &[String],
        receipts: &BTreeMap<String, Value>,
        error: DriverError,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        // Roll back what was provisioned, newest first, so a failed `up`
        // never leaves a half-provisioned database behind.
        let bindings = bindings_from(
            read_grant(&self.root, env_id, RESERVATION_GRANT).as_ref(),
            worktree,
            receipts.clone(),
        );
        let mut rollback_errors = Vec::new();
        for name in provisioned.iter().rev() {
            let Some(component) = snapshot.components.iter().find(|c| &c.name == name) else {
                continue;
            };
            let ctx = self.context(snapshot, component, &bindings, 1);
            match driver_for(component.runtime).destroy(&ctx, None) {
                Ok(()) => self.store.reset_component(env_id, name)?,
                // Left `created`, so `env down` destroys it again later.
                Err(rollback) => rollback_errors.push(
                    json!({"component": name, "code": rollback.code(), "message": rollback.to_string()}),
                ),
            }
        }
        self.release_capacity(env_id)?;
        let detail = json!({
            "code": error.code(),
            "message": error.to_string(),
            "rollback_errors": rollback_errors,
        });
        self.store.set_error(env_id, Some(&detail))?;
        let record = self
            .store
            .get(env_id)?
            .ok_or_else(|| StoreError::NotFound(env_id.into()))?;
        self.advance(
            &record,
            EnvironmentPhase::Failed,
            "provision failed",
            Some(detail),
        )?;
        Err(LifecycleError::Driver(error))
    }

    fn fail_start(
        &mut self,
        record: &EnvironmentRecord,
        snapshot: &CompiledEnvironment,
        bindings: &Bindings,
        started: Vec<(String, ComponentIdentity)>,
        failed: &str,
        error: DriverError,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        // Stop every component whose start returned an identity. Confirmed
        // stops can be settled; a failed stop keeps its exact identity and
        // running phase so `env down` can retry without losing ownership.
        let mut rollback_errors = Vec::new();
        for (name, identity) in started.iter().rev() {
            if let Some(component) = snapshot.components.iter().find(|c| &c.name == name) {
                let ctx = self.context(snapshot, component, bindings, 1);
                match driver_for(component.runtime).stop(&ctx, identity) {
                    Ok(()) => self.component_to(
                        &record.env_id,
                        name,
                        &[
                            ManagedServicePhase::RemovalPending,
                            ManagedServicePhase::Removed,
                            ManagedServicePhase::Settled,
                        ],
                        None,
                        None,
                    )?,
                    Err(stop) => {
                        append_log(
                            &ctx.log(),
                            &format!("[aethyme] rollback stop failed: {stop}\n"),
                        );
                        rollback_errors.push(json!({
                            "component": name,
                            "code": stop.code(),
                            "message": stop.to_string(),
                        }));
                    }
                }
            }
        }
        if !started.iter().any(|(name, _)| name == failed) {
            // The failing component never produced an identity; keep its
            // provisioned data but settle its start attempt for retry/down.
            self.component_to(
                &record.env_id,
                failed,
                &[
                    ManagedServicePhase::RemovalPending,
                    ManagedServicePhase::Removed,
                    ManagedServicePhase::Settled,
                ],
                None,
                None,
            )?;
        }
        if rollback_errors.is_empty() {
            self.release_capacity(&record.env_id)?;
        }
        let detail = json!({
            "code": error.code(),
            "component": failed,
            "message": error.to_string(),
            "rollback_errors": rollback_errors,
        });
        self.store.set_error(&record.env_id, Some(&detail))?;
        let current = self
            .store
            .get(&record.env_id)?
            .ok_or_else(|| StoreError::NotFound(record.env_id.clone()))?;
        self.advance(
            &current,
            EnvironmentPhase::Failed,
            "start failed",
            Some(detail),
        )?;
        Err(LifecycleError::Driver(error))
    }

    pub(super) fn release_capacity(&mut self, env_id: &str) -> Result<(), LifecycleError> {
        if let Some(grant) = read_grant(&self.root, env_id, CAPACITY_GRANT) {
            match self.coordinator.release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            ) {
                Ok(_) | Err(HostResourceError::LeaseNotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
            remove_grant(&self.root, env_id, CAPACITY_GRANT);
        }
        let reservation = self.store.get(env_id)?.and_then(|r| r.reservation_lease_id);
        self.store
            .set_leases(env_id, reservation.as_deref(), None)?;
        Ok(())
    }

    /// Destroy an environment. `abandon` skips the drivers and records what
    /// may have leaked; it exists for resources that are already gone or that
    /// an operator removed by hand.
    pub fn down(
        &mut self,
        env_id: &str,
        abandon: Option<&str>,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        let record = self.store.resolve(env_id)?;
        let _operation = self.operation_lock(&record.env_id)?;
        self.down_locked(&record.env_id, abandon)
    }

    fn down_locked(
        &mut self,
        env_id: &str,
        abandon: Option<&str>,
    ) -> Result<EnvironmentRecord, LifecycleError> {
        let record = self.store.resolve(env_id)?;
        let env_id = record.env_id.clone();
        if record.phase == EnvironmentPhase::Destroyed {
            return Ok(record);
        }
        let record = if record.phase == EnvironmentPhase::Destroying {
            record
        } else if EnvironmentPhase::Destroying.may_follow(record.phase) {
            self.advance(
                &record,
                EnvironmentPhase::Destroying,
                abandon.map_or("down", |_| "abandon"),
                None,
            )?
        } else {
            return Err(LifecycleError::WrongPhase {
                env_id,
                phase: record.phase.as_str(),
                next: "wait for the current operation to finish, then retry".into(),
            });
        };
        let snapshot = record.snapshot.clone();
        let components = self.store.components(&env_id)?;
        if let Some(reason) = abandon {
            let possibly_leaked: Vec<Value> = components
                .iter()
                .map(|c| json!({"component": c.component, "phase": c.phase, "identity": c.identity, "receipt": c.receipt}))
                .collect();
            self.release_all(&env_id)?;
            let detail = json!({"reason": reason, "possibly_leaked": possibly_leaked});
            let record = self.advance(
                &record,
                EnvironmentPhase::Destroyed,
                "abandoned",
                Some(detail),
            )?;
            crate::warn_unrecorded(
                "remove the environment directory",
                remove_environment_directory(&self.root, &env_id),
            );
            return Ok(record);
        }

        let (worktree, _export) = match self.destroy_source(&record) {
            Ok(found) => found,
            Err(reason) => {
                let detail = json!({"code": "snapshot_source_unavailable", "message": reason});
                self.store.set_error(&env_id, Some(&detail))?;
                return self
                    .advance(
                        &record,
                        EnvironmentPhase::DestroyFailed,
                        "destroy failed",
                        Some(detail),
                    )
                    .and_then(|_| {
                        Err(LifecycleError::Io(format!(
                            "{env_id}: cannot destroy without its source: {reason}"
                        )))
                    });
            }
        };
        let receipts: BTreeMap<String, Value> = components
            .iter()
            .filter_map(|c| c.receipt.clone().map(|r| (c.component.clone(), r)))
            .collect();
        let bindings = bindings_from(
            read_grant(&self.root, &env_id, RESERVATION_GRANT).as_ref(),
            &worktree,
            receipts,
        );
        for component in snapshot.components.iter().rev() {
            let stored = components.iter().find(|c| c.component == component.name);
            let identity: Option<ComponentIdentity> = stored
                .and_then(|c| c.identity.clone())
                .and_then(|v| serde_json::from_value(v).ok());
            let ctx = self.context(&snapshot, component, &bindings, 1);
            if let Err(error) = driver_for(component.runtime).destroy(&ctx, identity.as_ref()) {
                let detail = json!({
                    "code": error.code(),
                    "component": component.name,
                    "message": error.to_string(),
                });
                self.store.set_error(&env_id, Some(&detail))?;
                let current = self
                    .store
                    .get(&env_id)?
                    .ok_or_else(|| StoreError::NotFound(env_id.clone()))?;
                self.advance(
                    &current,
                    EnvironmentPhase::DestroyFailed,
                    "destroy failed",
                    Some(detail),
                )?;
                return Err(LifecycleError::Driver(error));
            }
        }
        self.release_all(&env_id)?;
        let current = self
            .store
            .get(&env_id)?
            .ok_or_else(|| StoreError::NotFound(env_id.clone()))?;
        let record = self.advance(&current, EnvironmentPhase::Destroyed, "destroyed", None)?;
        self.store.set_error(&env_id, None)?;
        crate::warn_unrecorded(
            "remove the environment directory",
            remove_environment_directory(&self.root, &env_id),
        );
        Ok(record)
    }

    fn release_all(&mut self, env_id: &str) -> Result<(), LifecycleError> {
        self.release_capacity(env_id)?;
        if let Some(grant) = read_grant(&self.root, env_id, RESERVATION_GRANT) {
            match self.coordinator.release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            ) {
                Ok(_) | Err(HostResourceError::LeaseNotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.store.set_leases(env_id, None, None)?;
        Ok(())
    }

    /// Where destroy hooks run: the worktree if it still exists, else a
    /// temporary export of the recorded commit. The second value keeps the
    /// export alive until destroy finishes.
    fn destroy_source(
        &self,
        record: &EnvironmentRecord,
    ) -> Result<(PathBuf, Option<tempfile::TempDir>), String> {
        // A shared environment runs from an Aethyme-made export, which has no
        // Git metadata; its marker proves it is the export of the recorded
        // commit (#1217).
        if let (Some(root), Some(commit)) = (&record.worktree_root, &record.snapshot.source.commit)
            && matches!(record.owner, super::compile::EnvironmentOwner::Ref { .. })
            && super::shared::is_shared_export_of(&self.root, Path::new(root), commit)
        {
            return Ok((PathBuf::from(root), None));
        }
        if let Some(root) = &record.worktree_root
            && Path::new(root).is_dir()
            && !matches!(record.owner, super::compile::EnvironmentOwner::Ref { .. })
        {
            // A path can be reused after manual checkout removal. Never run
            // recorded hooks in a replacement repository at the same name.
            if let Some(main_root) = &record.main_root {
                let expected = crate::git::GitRepo::worktree_common_dir(Path::new(main_root))
                    .map_err(|error| format!("cannot verify recorded repository: {error}"))?
                    .canonicalize()
                    .map_err(|error| format!("cannot resolve recorded repository: {error}"))?;
                let actual = crate::git::GitRepo::worktree_common_dir(Path::new(root))
                    .map_err(|error| format!("cannot verify current worktree: {error}"))?
                    .canonicalize()
                    .map_err(|error| format!("cannot resolve current worktree: {error}"))?;
                if actual != expected {
                    return Err("worktree path now belongs to a different repository".into());
                }
            }
            return Ok((PathBuf::from(root), None));
        }
        let needs_files = record
            .snapshot
            .components
            .iter()
            .any(|c| c.commands.destroy.is_some() || c.commands.stop.is_some());
        let export = tempfile::tempdir().map_err(|e| e.to_string())?;
        if !needs_files {
            return Ok((export.path().to_path_buf(), Some(export)));
        }
        let commit = record
            .snapshot
            .source
            .commit
            .as_deref()
            .ok_or("the snapshot records no commit")?;
        let main_root = record
            .main_root
            .as_deref()
            .ok_or("the environment records no repository root")?;
        export_commit(Path::new(main_root), commit, export.path())?;
        Ok((export.path().to_path_buf(), Some(export)))
    }

    /// Destroy environments whose worktree no longer exists, and retry
    /// failed destroys a bounded number of times. Returns ids it acted on.
    pub fn sweep_orphans(&mut self) -> Result<Vec<String>, LifecycleError> {
        let mut acted = Vec::new();
        for record in self.store.list(None)? {
            let orphaned = record.phase.retains_worktree()
                && record
                    .worktree_root
                    .as_deref()
                    .is_some_and(|root| !Path::new(root).exists());
            let retry = record.phase == EnvironmentPhase::DestroyFailed
                && self
                    .store
                    .transitions(&record.env_id, 200)?
                    .iter()
                    .filter(|t| t.to == EnvironmentPhase::DestroyFailed)
                    .count()
                    < MAX_DESTROY_RETRIES;
            if !(orphaned || retry) || record.phase == EnvironmentPhase::Destroying {
                continue;
            }
            acted.push(record.env_id.clone());
            // The outcome is recorded on the environment (destroyed or
            // destroy_failed with its error); keep sweeping the rest.
            crate::warn_unrecorded(
                &format!("destroy orphaned environment {}", record.env_id),
                self.down(&record.env_id, None),
            );
        }
        Ok(acted)
    }

    /// Renew leases that will expire within a third of their lifetime.
    pub fn renew_leases(&mut self) -> Result<(), LifecycleError> {
        for record in self.store.list(None)? {
            if !record.phase.retains_worktree() {
                continue;
            }
            for name in [RESERVATION_GRANT, CAPACITY_GRANT] {
                let Some(mut grant) = read_grant(&self.root, &record.env_id, name) else {
                    continue;
                };
                let ttl = if name == RESERVATION_GRANT {
                    super::compile::RESERVATION_TTL_SECONDS
                } else {
                    super::compile::CAPACITY_TTL_SECONDS
                };
                let remaining = grant.lease.expires_at - now_ms() / 1000;
                if remaining > (ttl as i64) * 2 / 3 {
                    continue;
                }
                if let Ok(lease) = self.coordinator.renew(
                    &grant.lease.lease_id,
                    grant.lease.generation,
                    &grant.ownership_token,
                    ttl,
                ) {
                    grant.lease = lease;
                    write_grant(&self.root, &record.env_id, name, &grant)?;
                }
            }
        }
        Ok(())
    }
}

/// Export `commit` from the repository at `main_root` into `into` without
/// registering a worktree.
pub(super) fn export_commit(main_root: &Path, commit: &str, into: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let mut archive = Command::new("git")
        .args(["archive", "--format=tar", commit])
        .current_dir(main_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("git archive: {e}"))?;
    let stdout = archive
        .stdout
        .take()
        .ok_or("git archive produced no output")?;
    let status = Command::new("tar")
        .args(["-x", "-f", "-"])
        .current_dir(into)
        .stdin(stdout)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    let archived = archive.wait_with_output().map_err(|e| e.to_string())?;
    if !archived.status.success() {
        return Err(format!(
            "git archive {commit} failed: {}",
            String::from_utf8_lossy(&archived.stderr).trim()
        ));
    }
    if !status.success() {
        return Err(format!("tar exited {status}"));
    }
    Ok(())
}

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The id of the environment that owns `lease_id`, if any. Generic lease
/// release and reconcile refuse such leases.
pub fn environment_owning_lease(
    store_path: &Path,
    lease_id: &str,
) -> Result<Option<String>, super::store::StoreError> {
    let Some(store) = EnvironmentStore::open_read_only(store_path)? else {
        return Ok(None);
    };
    Ok(store
        .list(None)?
        .into_iter()
        .find(|r| {
            r.reservation_lease_id.as_deref() == Some(lease_id)
                || r.capacity_lease_id.as_deref() == Some(lease_id)
        })
        .map(|r| r.env_id))
}

/// Destroy every live environment of `worktree` before it is removed.
/// `Err` names what could not be destroyed; the caller must not remove the
/// worktree. A host with no environment store is a no-op.
pub fn destroy_for_worktree_with(
    store_path: &Path,
    host_db: &Path,
    root: &Path,
    worktree: &Path,
) -> Result<Vec<String>, String> {
    let fingerprint = crate::console::worktree_fingerprint(worktree);
    let Some(reader) =
        EnvironmentStore::open_read_only(store_path).map_err(|error| error.to_string())?
    else {
        return Ok(Vec::new());
    };
    let live: Vec<String> = reader
        .list(None)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|r| r.worktree_fingerprint == fingerprint && r.phase.retains_worktree())
        .map(|r| r.env_id)
        .collect();
    drop(reader);
    if live.is_empty() {
        return Ok(Vec::new());
    }
    let mut lifecycle = Lifecycle {
        store: EnvironmentStore::open(store_path).map_err(|e| e.to_string())?,
        coordinator: HostResourceCoordinator::open(host_db).map_err(|e| e.to_string())?,
        root: root.to_path_buf(),
        holder_pid: None,
        hook_timeout: super::exec::DEFAULT_HOOK_TIMEOUT,
        actor: "worktree-removal".into(),
    };
    let mut failed = Vec::new();
    for env_id in &live {
        if let Err(error) = lifecycle.down(env_id, None) {
            failed.push(format!("{env_id}: {error}"));
        }
    }
    if failed.is_empty() {
        Ok(live)
    } else {
        Err(format!(
            "environments of this worktree could not be destroyed, so it was kept: {}; fix the cause and run `aethyme env destroy <id> --retry`, or `aethyme env destroy <id> --abandon --reason \"...\"`",
            failed.join("; ")
        ))
    }
}

/// [`destroy_for_worktree_with`] on the default host paths.
pub fn destroy_for_worktree(worktree: &Path) -> Result<Vec<String>, String> {
    let store = super::store::default_store_path().map_err(|error| error.to_string())?;
    let host_db =
        crate::resources::default_host_resource_db_path().map_err(|error| error.to_string())?;
    let root = super::logs::environments_directory()
        .ok_or_else(|| "host state directory is unavailable".to_owned())?;
    destroy_for_worktree_with(&store, &host_db, &root, worktree)
}
