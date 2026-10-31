//! Compile a validated profile into a self-contained snapshot (#1209).
//!
//! The snapshot is what every later lifecycle step runs from: stop, start,
//! destroy and a wake triggered by an HTTP request all read the commands
//! recorded here, never the file as it is now. A branch that edits or deletes
//! its own `destroy` command therefore cannot strand what it created, and a
//! network-triggered wake can only run commands that were in force when the
//! environment was brought up.
//!
//! A profile also compiles to two host-resource requests:
//!
//! - the **reservation** (namespace, one port per listening component), held
//!   for the environment's whole life, and
//! - the **capacity** (pool units), held only while the environment runs.
//!
//! Parking releases the second and keeps the first, so no lease ever holds a
//! partial allocation and lease states stay `active | quarantined | released`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::spec::{
    ComponentSpec, Engine, EnvironmentsFile, HealthSpec, HostEnvironmentsFile, PoolScope, Preempt,
    Runtime,
};
use super::validate::{declared_engine, effective_rank, env_suffix, pool_limit, profile_pool};
use crate::resources::{
    HOST_RESOURCE_REQUEST_SCHEMA_VERSION, HostResourceKind, HostResourceRequest,
    HostResourceRequirement,
};

pub const COMPILED_SCHEMA: &str = "aethyme.environment-snapshot/v1";

/// Reservation leases are renewed by the supervisor; this bounds how long a
/// reservation outlives a supervisor that died without destroying anything.
pub const RESERVATION_TTL_SECONDS: u64 = 3_600;
/// Capacity leases are renewed while the environment runs.
pub const CAPACITY_TTL_SECONDS: u64 = 300;

pub const NAMESPACE_KEY: &str = "namespace";
pub const CAPACITY_KEY: &str = "capacity";

/// Who an environment belongs to. Part of its identity: the same profile for
/// two owners is two environments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnvironmentOwner {
    /// A worktree, identified by its opaque fingerprint. Sessions resolve to
    /// their worktree, so a session and its checkout share environments.
    Worktree { fingerprint: String },
    /// A ref, for host-scoped shared environments (E9).
    Ref { name: String },
}

impl EnvironmentOwner {
    fn identity(&self) -> String {
        match self {
            Self::Worktree { fingerprint } => format!("worktree:{fingerprint}"),
            Self::Ref { name } => format!("ref:{name}"),
        }
    }
}

/// Inputs that do not come from the file.
#[derive(Debug, Clone)]
pub struct CompileContext {
    /// The repository key gates and consoles use (`git_origin_fingerprint`).
    pub repository: String,
    pub owner: EnvironmentOwner,
    pub worktree_fingerprint: String,
    /// Commit the file was read at; destroy falls back to a checkout of it
    /// when the worktree is gone.
    pub source_commit: Option<String>,
    /// Whether the file differed from `source_commit` when read.
    pub source_dirty: bool,
    /// Digest of the file bytes as read.
    pub source_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledEnvironment {
    pub schema: String,
    pub env_id: String,
    pub repository: String,
    pub owner: EnvironmentOwner,
    pub worktree_fingerprint: String,
    pub profile: String,
    pub rank: u32,
    pub priority: i32,
    pub units: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<CompiledPool>,
    pub idle_seconds: u64,
    pub route_domain: String,
    pub port_range: (u16, u16),
    pub uses_shared: Vec<String>,
    /// Topological order: every component follows its dependencies.
    pub components: Vec<CompiledComponent>,
    pub source: SnapshotSource,
    /// Digest of everything above except `source`, so committing an unchanged
    /// file does not look like a reconfiguration.
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSource {
    pub file: String,
    pub file_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledPool {
    /// The file's pool name.
    pub name: String,
    /// The host-lease pool: repository pools are scoped to the repository so
    /// two repositories never share or disagree about a limit by accident.
    pub lease_pool: String,
    pub scope: PoolScope,
    pub limit: u32,
    pub preempt: Preempt,
    #[serde(default = "default_min_active_seconds")]
    pub min_active_seconds: u64,
}

fn default_min_active_seconds() -> u64 {
    super::spec::DEFAULT_MIN_ACTIVE_SECONDS
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledComponent {
    pub name: String,
    pub runtime: Runtime,
    pub depends_on: Vec<String>,
    pub commands: ComponentCommands,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthSpec>,
    /// Resource key of this component's port in the reservation lease.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port_key: Option<String>,
    pub routed: bool,
    pub env: BTreeMap<String, String>,
    pub pass_env: Vec<String>,
    pub stop_grace_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<Engine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compose_file: Option<String>,
    pub services: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serve_dir: Option<String>,
    // Omitted when false so existing snapshots keep their digest.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spa_fallback: bool,
    // Omitted when unset so existing snapshots keep their digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_path: Option<String>,
    pub cache_inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_output: Option<String>,
    pub cache_env: Vec<String>,
    pub template_inputs: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentCommands {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destroy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_destroy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_probe: Option<String>,
}

/// Default grace between SIGTERM and SIGKILL when stopping a process group.
pub const DEFAULT_STOP_GRACE_SECONDS: u64 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    UnknownProfile(String),
    /// The file has validation errors; compile only runs on a valid file.
    Invalid,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProfile(name) => write!(f, "profile {name:?} is not declared"),
            Self::Invalid => f.write_str("the environments file has validation errors"),
        }
    }
}

/// `env-<12 hex>`: stable for one (repository, owner, profile), so the
/// namespace and ports survive park, wake and supervisor restarts.
pub fn environment_id(repository: &str, owner: &EnvironmentOwner, profile: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"aethyme-environment-id/v1\0");
    for part in [repository, &owner.identity(), profile] {
        digest.update(part.as_bytes());
        digest.update(b"\0");
    }
    format!("env-{}", &format!("{:x}", digest.finalize())[..12])
}

/// The profile name the file selects when none is given.
pub fn default_profile_name(file: &EnvironmentsFile) -> Option<&str> {
    file.default_profile
        .as_deref()
        .or_else(|| file.profiles.first().map(|p| p.name.as_str()))
}

pub fn compile(
    file: &EnvironmentsFile,
    host: &HostEnvironmentsFile,
    profile_name: &str,
    file_label: &str,
    context: &CompileContext,
) -> Result<CompiledEnvironment, CompileError> {
    let (index, profile) = file
        .profiles
        .iter()
        .enumerate()
        .find(|(_, p)| p.name == profile_name)
        .ok_or_else(|| CompileError::UnknownProfile(profile_name.to_string()))?;
    let order = topological_order(&profile.components).ok_or(CompileError::Invalid)?;
    let pool = if profile.units > 0 {
        let name = profile_pool(file, profile).ok_or(CompileError::Invalid)?;
        let spec = file.pools.get(name).ok_or(CompileError::Invalid)?;
        let limit = pool_limit(file, host, name).ok_or(CompileError::Invalid)?;
        let lease_pool = match spec.scope {
            PoolScope::Repository => format!("env:{}:{name}", context.repository),
            PoolScope::Host => format!("env:host:{name}"),
        };
        Some(CompiledPool {
            name: name.to_string(),
            lease_pool,
            scope: spec.scope,
            limit,
            preempt: spec.preempt,
            min_active_seconds: spec.min_active_seconds,
        })
    } else {
        None
    };
    let components = order
        .into_iter()
        .map(|component| compile_component(file, component))
        .collect();
    let env_id = environment_id(&context.repository, &context.owner, &profile.name);
    let mut compiled = CompiledEnvironment {
        schema: COMPILED_SCHEMA.into(),
        env_id,
        repository: context.repository.clone(),
        owner: context.owner.clone(),
        worktree_fingerprint: context.worktree_fingerprint.clone(),
        profile: profile.name.clone(),
        rank: effective_rank(index, profile),
        priority: profile.priority,
        units: profile.units,
        pool,
        idle_seconds: profile.idle_seconds.unwrap_or(file.default_idle_seconds),
        route_domain: file.route_domain.clone(),
        port_range: (file.ports.start, file.ports.end),
        uses_shared: profile.uses_shared.clone(),
        components,
        source: SnapshotSource {
            file: file_label.to_string(),
            file_digest: context.source_digest.clone(),
            commit: context.source_commit.clone(),
            dirty: context.source_dirty,
        },
        digest: String::new(),
    };
    compiled.digest = snapshot_digest(&compiled);
    Ok(compiled)
}

fn compile_component(file: &EnvironmentsFile, component: &ComponentSpec) -> CompiledComponent {
    let engine = match component.runtime {
        Runtime::Compose | Runtime::Container => declared_engine(file, component),
        _ => None,
    };
    CompiledComponent {
        name: component.name.clone(),
        runtime: component.runtime,
        depends_on: component.depends_on.clone(),
        commands: ComponentCommands {
            build: component.build.clone(),
            provision: component.provision.clone(),
            start: component.start.clone(),
            stop: component.stop.clone(),
            destroy: component.destroy.clone(),
            template: component.template.clone(),
            template_destroy: component.template_destroy.clone(),
            activity_probe: component.activity_probe.clone(),
        },
        health: component.health.clone(),
        port_key: component.listen.then(|| port_key(&component.name)),
        routed: component.routed(),
        env: component.env.clone(),
        pass_env: component.pass_env.clone(),
        stop_grace_seconds: component
            .stop_grace_seconds
            .unwrap_or(DEFAULT_STOP_GRACE_SECONDS),
        engine,
        compose_file: component.file.clone(),
        services: component.services.clone(),
        image: component.image.clone(),
        args: component.args.clone(),
        container_port: component.container_port,
        serve_dir: component.serve_dir.clone(),
        spa_fallback: component.spa_fallback,
        entry_path: component.entry_path.clone(),
        cache_inputs: component.cache_inputs.clone(),
        build_output: component.build_output.clone(),
        cache_env: component.cache_env.clone(),
        template_inputs: component.template_inputs.clone(),
    }
}

pub fn port_key(component: &str) -> String {
    format!("port_{component}")
}

/// Kahn's algorithm, ties broken by declaration order so the result is
/// deterministic. `None` on a cycle or a dangling dependency.
pub fn topological_order(components: &[ComponentSpec]) -> Option<Vec<&ComponentSpec>> {
    let mut remaining: Vec<&ComponentSpec> = components.iter().collect();
    let mut placed: Vec<&ComponentSpec> = Vec::with_capacity(components.len());
    while !remaining.is_empty() {
        let ready = remaining.iter().position(|candidate| {
            candidate
                .depends_on
                .iter()
                .all(|dependency| placed.iter().any(|done| &done.name == dependency))
        })?;
        placed.push(remaining.remove(ready));
    }
    Some(placed)
}

fn snapshot_digest(compiled: &CompiledEnvironment) -> String {
    let mut normalized = compiled.clone();
    normalized.digest = String::new();
    normalized.source = SnapshotSource {
        file: String::new(),
        file_digest: String::new(),
        commit: None,
        dirty: false,
    };
    let bytes = serde_json::to_vec(&normalized).unwrap_or_default();
    format!("{:x}", Sha256::digest(bytes))
}

impl CompiledEnvironment {
    /// Namespace plus one port per listening component. Held from creation
    /// until destroy is confirmed.
    ///
    /// `instance` makes the request id unique per acquisition: the
    /// coordinator never reuses a request id once its lease is released, so
    /// a re-created environment (or a second wake) needs a new one.
    pub fn reservation_request(
        &self,
        holder_pid: Option<u32>,
        instance: u64,
    ) -> HostResourceRequest {
        let mut resources = vec![HostResourceRequirement {
            key: NAMESPACE_KEY.into(),
            resource: HostResourceKind::Namespace {
                prefix: self.env_id.clone(),
            },
        }];
        for component in &self.components {
            if let Some(key) = &component.port_key {
                resources.push(HostResourceRequirement {
                    key: key.clone(),
                    resource: HostResourceKind::TcpPort {
                        start: self.port_range.0,
                        end: self.port_range.1,
                    },
                });
            }
        }
        HostResourceRequest {
            schema_version: HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: format!("{}:reservation:{instance}", self.env_id),
            repository: self.repository.clone(),
            worktree_fingerprint: self.worktree_fingerprint.clone(),
            run_id: self.env_id.clone(),
            ttl_seconds: RESERVATION_TTL_SECONDS,
            holder_pid,
            resources,
        }
    }

    /// Pool units, held only while the environment runs. `None` for a
    /// zero-unit profile, which never waits for capacity.
    pub fn capacity_request(
        &self,
        holder_pid: Option<u32>,
        instance: u64,
    ) -> Option<HostResourceRequest> {
        let pool = self.pool.as_ref()?;
        Some(HostResourceRequest {
            schema_version: HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: format!("{}:capacity:{instance}", self.env_id),
            repository: self.repository.clone(),
            worktree_fingerprint: self.worktree_fingerprint.clone(),
            run_id: self.env_id.clone(),
            ttl_seconds: CAPACITY_TTL_SECONDS,
            holder_pid,
            resources: vec![HostResourceRequirement {
                key: CAPACITY_KEY.into(),
                resource: HostResourceKind::Capacity {
                    pool: pool.lease_pool.clone(),
                    units: self.units,
                    limit: pool.limit,
                },
            }],
        })
    }

    /// Variable names a component's commands receive, with the values known
    /// at compile time. Ports and URLs are filled in once leases are granted.
    pub fn variable_names(&self, component: &CompiledComponent) -> Vec<String> {
        let mut names = vec![
            "AETHYME_ENV_ID".to_string(),
            "AETHYME_ENV_NAMESPACE".into(),
            "AETHYME_ENV_PROFILE".into(),
            "AETHYME_ENV_WORKTREE".into(),
            "AETHYME_ENV_HOOK_ATTEMPT".into(),
        ];
        for other in &self.components {
            if other.port_key.is_some() {
                names.push(format!("AETHYME_ENV_PORT_{}", env_suffix(&other.name)));
                names.push(format!("AETHYME_ENV_URL_{}", env_suffix(&other.name)));
            }
            names.push(format!("AETHYME_ENV_RECEIPT_{}", env_suffix(&other.name)));
        }
        names.extend(component.env.keys().cloned());
        names.extend(component.pass_env.iter().cloned());
        names.sort();
        names.dedup();
        names
    }
}

/// A listening component's URL: loopback, its port, then its `entry_path`.
pub fn component_url(port: impl std::fmt::Display, entry_path: Option<&str>) -> String {
    format!("http://127.0.0.1:{port}{}", entry_path.unwrap_or(""))
}
