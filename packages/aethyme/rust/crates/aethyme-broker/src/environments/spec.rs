//! The `.aethyme/environments.toml` schema (#1209).
//!
//! These types are the parsed file and nothing more: no defaults are resolved
//! here beyond serde's, and nothing is checked across fields. Cross-field rules
//! live in `validate`, resolution in `compile`. Every table denies unknown
//! fields, so a misspelt key is a diagnostic rather than a silently ignored
//! setting.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The only schema this binary reads.
pub const ENVIRONMENTS_SCHEMA: u32 = 1;

/// Default idle threshold. Long enough to cover an agent's edit-test cycle.
pub const DEFAULT_IDLE_SECONDS: u64 = 900;

/// Default host port range for listening components. Disjoint from the
/// console's 4173..=4199 so the two features never compete for a port.
pub const DEFAULT_PORT_START: u16 = 42_000;
pub const DEFAULT_PORT_END: u16 = 42_999;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentsFile {
    pub schema: u32,
    #[serde(default)]
    pub default_profile: Option<String>,
    #[serde(default = "default_idle_seconds")]
    pub default_idle_seconds: u64,
    #[serde(default = "default_route_domain")]
    pub route_domain: String,
    #[serde(default)]
    pub ports: PortRange,
    #[serde(default)]
    pub engine: EngineConfig,
    #[serde(default)]
    pub pools: BTreeMap<String, PoolSpec>,
    #[serde(default)]
    pub profiles: Vec<ProfileSpec>,
    /// E8: path rules giving a minimum profile. Validated here, applied later.
    #[serde(default)]
    pub rules: Vec<RuleSpec>,
    /// E9: host-scoped environments owned by a ref. Validated here.
    #[serde(default)]
    pub shared: Vec<SharedSpec>,
}

fn default_idle_seconds() -> u64 {
    DEFAULT_IDLE_SECONDS
}

fn default_route_domain() -> String {
    "localhost".into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
}

impl Default for PortRange {
    fn default() -> Self {
        Self {
            start: DEFAULT_PORT_START,
            end: DEFAULT_PORT_END,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineConfig {
    /// Container engine for `compose` and `container` components.
    #[serde(default)]
    pub default: Option<Engine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Docker,
    Podman,
    /// Apple's `container`: one lightweight VM per container. No compose.
    Container,
    Nerdctl,
}

impl Engine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
            Self::Container => "container",
            Self::Nerdctl => "nerdctl",
        }
    }

    pub fn supports_compose(self) -> bool {
        !matches!(self, Self::Container)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolSpec {
    /// Required for repository pools. Forbidden for host pools, whose limit
    /// comes only from host state, so repositories cannot disagree about it.
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub scope: PoolScope,
    #[serde(default)]
    pub preempt: Preempt,
    /// `park_lru` never parks an environment active for less than this,
    /// so two agents cannot keep parking each other (#1215).
    #[serde(default = "default_min_active_seconds")]
    pub min_active_seconds: u64,
}

pub const DEFAULT_MIN_ACTIVE_SECONDS: u64 = 120;

fn default_min_active_seconds() -> u64 {
    DEFAULT_MIN_ACTIVE_SECONDS
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolScope {
    #[default]
    Repository,
    Host,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preempt {
    #[default]
    None,
    ParkLru,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSpec {
    pub name: String,
    /// Total order for raise-only inference. Defaults to declaration order.
    #[serde(default)]
    pub rank: Option<u32>,
    /// Capacity units held while active.
    #[serde(default)]
    pub units: u32,
    #[serde(default)]
    pub pool: Option<String>,
    #[serde(default)]
    pub idle_seconds: Option<u64>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub uses_shared: Vec<String>,
    #[serde(default)]
    pub components: Vec<ComponentSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    Process,
    Compose,
    Container,
    Static,
    Hooks,
}

impl Runtime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Compose => "compose",
            Self::Container => "container",
            Self::Static => "static",
            Self::Hooks => "hooks",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentSpec {
    pub name: String,
    pub runtime: Runtime,
    #[serde(default)]
    pub depends_on: Vec<String>,
    // Commands, each run as `sh -c` from the worktree root.
    #[serde(default)]
    pub build: Option<String>,
    #[serde(default)]
    pub provision: Option<String>,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub stop: Option<String>,
    #[serde(default)]
    pub destroy: Option<String>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub template_destroy: Option<String>,
    #[serde(default)]
    pub activity_probe: Option<String>,
    #[serde(default)]
    pub health: Option<HealthSpec>,
    #[serde(default)]
    pub listen: bool,
    /// Defaults to true for `static`, false otherwise.
    #[serde(default)]
    pub route: Option<bool>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub pass_env: Vec<String>,
    #[serde(default)]
    pub stop_grace_seconds: Option<u64>,
    // Driver-specific fields.
    /// `compose`: the compose file, relative to the worktree root.
    #[serde(default)]
    pub file: Option<String>,
    /// `compose`: services to start; all when empty.
    #[serde(default)]
    pub services: Vec<String>,
    /// `container`: the image reference.
    #[serde(default)]
    pub image: Option<String>,
    /// `container`: arguments appended after the image.
    #[serde(default)]
    pub args: Vec<String>,
    /// `container`: the port inside the container that `listen` maps to.
    #[serde(default)]
    pub container_port: Option<u16>,
    /// `compose` / `container`: overrides `[engine] default`.
    #[serde(default)]
    pub engine: Option<Engine>,
    /// `static`: the directory served, relative to the worktree root.
    #[serde(default)]
    pub serve_dir: Option<String>,
    /// The page to open for a listening process, compose or container component.
    /// Appended to that component's URL (`/o/`, #1292).
    #[serde(default)]
    pub entry_path: Option<String>,
    /// E11: build cache inputs.
    #[serde(default)]
    pub cache_inputs: Vec<String>,
    /// E11: the directory `build` produces, cached by input digest.
    #[serde(default)]
    pub build_output: Option<String>,
    #[serde(default)]
    pub cache_env: Vec<String>,
    #[serde(default)]
    pub template_inputs: Vec<String>,
}

impl ComponentSpec {
    pub fn routed(&self) -> bool {
        self.route.unwrap_or(self.runtime == Runtime::Static)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthSpec {
    /// HTTP path probed on the component's port, e.g. `/healthz`.
    #[serde(default)]
    pub http: Option<String>,
    /// A TCP connect to the component's port.
    #[serde(default)]
    pub tcp: Option<bool>,
    /// A command that exits 0 when healthy.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default = "default_health_timeout")]
    pub timeout_seconds: u64,
    #[serde(default = "default_health_interval")]
    pub interval_ms: u64,
}

fn default_health_timeout() -> u64 {
    60
}

fn default_health_interval() -> u64 {
    500
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSpec {
    pub name: String,
    pub paths: Vec<String>,
    pub min_profile: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedSpec {
    pub name: String,
    pub profile: String,
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub idle_seconds: Option<u64>,
    #[serde(default)]
    pub min_redeploy_interval_seconds: Option<u64>,
}

/// Host-scoped settings: `<host state>/environments.toml`. Only host pool
/// limits live here today.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostEnvironmentsFile {
    #[serde(default)]
    pub pools: BTreeMap<String, HostPoolSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostPoolSpec {
    pub limit: u32,
}
