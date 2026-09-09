//! One console per repository, or one per worktree.
//!
//! Agent isolation and operator singularity are different problems that want
//! opposite defaults, and conflating them is what produces two local URLs that
//! both answer while serving different revisions. An agent needs its own
//! worktree so two sessions cannot corrupt each other's tree. An operator needs
//! exactly one console, because a browser tab carries no evidence of which
//! checkout is behind it and an HTTP 200 is not proof that the intended code is
//! running.
//!
//! Which default is right depends on the repository, not on the broker. A light
//! dev server wants one console for the whole repository; an expensive one wants
//! a console per worktree sharing the heavy backing services. Those are not two
//! architectures -- they are the same four host resource kinds composed
//! differently:
//!
//! | mode           | reserves                                             |
//! |----------------|------------------------------------------------------|
//! | `singular`     | an exclusive key, and one fixed port                  |
//! | `per_worktree` | a port from a range, a private namespace, a pool slot |
//! | `unmanaged`    | nothing                                               |
//!
//! `unmanaged` is a named mode rather than an `--allow-parallel` flag on the
//! others. A flag that silently disables coordination reads as an option; a mode
//! the repository had to select reads as a decision, and shows up in status
//! output as one.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::resources::{
    HOST_RESOURCE_REQUEST_SCHEMA_VERSION, HostResourceKind, HostResourceRequest,
    HostResourceRequirement,
};

/// Port for `singular`, and the base of the range for `per_worktree`.
pub const DEFAULT_CONSOLE_PORT: u16 = 4173;
/// Top of the default `per_worktree` range: 27 concurrent worktrees is far more
/// than a human operates, and a bounded range keeps the allocation legible.
pub const DEFAULT_CONSOLE_PORT_END: u16 = 4199;
/// Concurrent `per_worktree` consoles before the pool refuses. The limit exists
/// so an abandoned fleet cannot quietly exhaust the machine.
pub const DEFAULT_CONSOLE_POOL_LIMIT: u32 = 4;
/// Lease lifetime. A supervised console renews while it runs, so this only
/// bounds how long a crashed holder's reservation survives.
pub const DEFAULT_CONSOLE_TTL_SECONDS: u64 = 900;

/// Resource keys. These become `AETHYME_RESOURCE_PORT`, `_NAMESPACE`, `_SLOT`
/// and `_CONSOLE` in the served process, which is the contract a dev server
/// reads to report what it is serving.
pub const CONSOLE_PORT_KEY: &str = "port";
pub const CONSOLE_NAMESPACE_KEY: &str = "namespace";
pub const CONSOLE_SLOT_KEY: &str = "slot";
pub const CONSOLE_EXCLUSIVE_KEY: &str = "console";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleMode {
    Singular,
    PerWorktree,
    Unmanaged,
}

impl ConsoleMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Singular => "singular",
            Self::PerWorktree => "per_worktree",
            Self::Unmanaged => "unmanaged",
        }
    }

    /// `per-worktree` is accepted alongside `per_worktree`: the CLI spells
    /// multi-word names with hyphens and TOML keys with underscores, and an
    /// operator copying between them should not have to notice.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "singular" => Some(Self::Singular),
            "per_worktree" | "per-worktree" => Some(Self::PerWorktree),
            "unmanaged" => Some(Self::Unmanaged),
            _ => None,
        }
    }
}

/// `[console]` section of `.aethyme/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleConfig {
    pub mode: ConsoleMode,
    pub port_start: u16,
    pub port_end: u16,
    pub pool_limit: u32,
    pub ttl_seconds: u64,
}

impl Default for ConsoleConfig {
    /// Singular by default. The failure it prevents -- two URLs serving
    /// different revisions -- is silent and costs real debugging time, while
    /// the cost of defaulting wrong for a large repository is one config line.
    fn default() -> Self {
        Self {
            mode: ConsoleMode::Singular,
            port_start: DEFAULT_CONSOLE_PORT,
            port_end: DEFAULT_CONSOLE_PORT_END,
            pool_limit: DEFAULT_CONSOLE_POOL_LIMIT,
            ttl_seconds: DEFAULT_CONSOLE_TTL_SECONDS,
        }
    }
}

impl ConsoleConfig {
    /// Unreadable, unparseable and unrecognised values all fall back to the
    /// default, matching `PromoteConfig::load`. A console mode is not worth
    /// failing a command over, and a repository that has never configured one
    /// is the common case rather than an error.
    pub fn load(main_root: &Path) -> Self {
        let mut config = Self::default();
        let Ok(text) = std::fs::read_to_string(main_root.join(".aethyme/config.toml")) else {
            return config;
        };
        let Ok(value) = text.parse::<toml::Value>() else {
            return config;
        };
        let Some(console) = value.get("console") else {
            return config;
        };
        if let Some(mode) = console
            .get("mode")
            .and_then(|v| v.as_str())
            .and_then(ConsoleMode::parse)
        {
            config.mode = mode;
        }
        if let Some(port) = console.get("port").and_then(port_value) {
            config.port_start = port;
        }
        if let Some(port) = console.get("port_end").and_then(port_value) {
            config.port_end = port;
        }
        if let Some(limit) = console
            .get("pool_limit")
            .and_then(|v| v.as_integer())
            .and_then(|v| u32::try_from(v).ok())
            .filter(|limit| *limit > 0)
        {
            config.pool_limit = limit;
        }
        if let Some(ttl) = console
            .get("ttl_seconds")
            .and_then(|v| v.as_integer())
            .and_then(|v| u64::try_from(v).ok())
            .filter(|ttl| *ttl > 0)
        {
            config.ttl_seconds = ttl;
        }
        // A range configured backwards would otherwise reach the allocator as
        // an empty range and refuse every console with a resource conflict,
        // which reads as contention rather than as the typo it is.
        if config.port_end < config.port_start {
            config.port_end = config.port_start;
        }
        config
    }

    /// The range actually reserved.
    ///
    /// `singular` pins the single configured port rather than allocating from
    /// the range. A stable URL is the whole point of that mode -- a console the
    /// operator cannot bookmark has not replaced the ambiguity it was meant to
    /// remove.
    pub fn effective_port_range(&self) -> (u16, u16) {
        match self.mode {
            ConsoleMode::Singular => (self.port_start, self.port_start),
            _ => (self.port_start, self.port_end),
        }
    }
}

fn port_value(value: &toml::Value) -> Option<u16> {
    value
        .as_integer()
        .and_then(|v| u16::try_from(v).ok())
        .filter(|port| *port > 0)
}

/// Opaque digest of a worktree path, matching how gates and preparation
/// fingerprint a checkout. Absolute paths are never persisted host-side.
pub fn worktree_fingerprint(worktree_root: &Path) -> String {
    format!(
        "{:x}",
        Sha256::digest(worktree_root.to_string_lossy().as_bytes())
    )
}

/// Short, human-facing form of a fingerprint, for namespaces and status output.
fn short_fingerprint(fingerprint: &str) -> String {
    fingerprint.chars().take(12).collect()
}

/// What a console would reserve, or `None` under `unmanaged`.
///
/// `None` is the honest answer rather than an empty request: a request with no
/// requirements still creates a lease, and a lease that reserves nothing would
/// appear in the inventory as a console under coordination when the repository
/// has explicitly opted out of it.
pub fn console_request(
    config: &ConsoleConfig,
    repository: &str,
    worktree_root: &Path,
    run_id: &str,
    holder_pid: Option<u32>,
) -> Option<HostResourceRequest> {
    let fingerprint = worktree_fingerprint(worktree_root);
    let (start, end) = config.effective_port_range();
    let resources = match config.mode {
        ConsoleMode::Unmanaged => return None,
        // The exclusive key is what makes a second launch fail instead of
        // succeeding onto another port. The port is pinned beside it so the
        // canonical URL never moves.
        ConsoleMode::Singular => vec![
            HostResourceRequirement {
                key: CONSOLE_EXCLUSIVE_KEY.into(),
                resource: HostResourceKind::ExclusiveKey {
                    name: format!("console:{repository}"),
                },
            },
            HostResourceRequirement {
                key: CONSOLE_PORT_KEY.into(),
                resource: HostResourceKind::TcpPort { start, end },
            },
        ],
        // A port so the consoles do not collide, a namespace so their backing
        // state does not, and a pool slot so an abandoned fleet cannot exhaust
        // the machine. The heavy shared services stay singular under their own
        // long-lived lease; each console namespaces inside them.
        ConsoleMode::PerWorktree => vec![
            HostResourceRequirement {
                key: CONSOLE_PORT_KEY.into(),
                resource: HostResourceKind::TcpPort { start, end },
            },
            HostResourceRequirement {
                key: CONSOLE_NAMESPACE_KEY.into(),
                resource: HostResourceKind::Namespace {
                    prefix: format!("console-{}", short_fingerprint(&fingerprint)),
                },
            },
            HostResourceRequirement {
                key: CONSOLE_SLOT_KEY.into(),
                resource: HostResourceKind::Capacity {
                    pool: format!("console:{repository}"),
                    units: 1,
                    limit: config.pool_limit,
                },
            },
        ],
    };
    Some(HostResourceRequest {
        schema_version: HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
        request_id: format!("console-{}-{run_id}", short_fingerprint(&fingerprint)),
        repository: repository.to_string(),
        worktree_fingerprint: fingerprint,
        run_id: run_id.to_string(),
        ttl_seconds: config.ttl_seconds,
        holder_pid,
        resources,
    })
}

/// Which checkout a console is served from, and whether that is the canonical
/// one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConsoleIdentity {
    pub repository: String,
    pub mode: ConsoleMode,
    pub worktree_fingerprint: String,
    /// True when the console is served from the primary checkout. A console
    /// served from an agent worktree is legitimate but not canonical, and
    /// saying so is the difference between a URL and a trustworthy URL.
    pub canonical: bool,
}

pub fn console_identity(
    config: &ConsoleConfig,
    repository: &str,
    main_root: &Path,
    worktree_root: &Path,
) -> ConsoleIdentity {
    // Compare resolved paths: a symlinked or `/private`-prefixed spelling of
    // the primary checkout is still the primary checkout, and reporting it as
    // an agent worktree would train the operator to ignore the warning.
    let resolve = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    ConsoleIdentity {
        repository: repository.to_string(),
        mode: config.mode,
        worktree_fingerprint: worktree_fingerprint(worktree_root),
        canonical: resolve(main_root) == resolve(worktree_root),
    }
}

/// The console leases this repository currently holds.
///
/// Selection is by resource key rather than by lease naming, so a console is
/// whatever actually reserved a console resource -- an inventory that agreed
/// with the naming convention but not with the reservations would be exactly
/// the kind of plausible-looking evidence this is meant to replace.
pub fn console_leases(
    leases: &[crate::resources::HostResourceLease],
    repository: &str,
) -> Vec<crate::resources::HostResourceLease> {
    leases
        .iter()
        .filter(|lease| lease.repository == repository)
        .filter(|lease| {
            lease.allocations.iter().any(|allocation| {
                matches!(
                    allocation.key.as_str(),
                    CONSOLE_EXCLUSIVE_KEY | CONSOLE_PORT_KEY
                )
            })
        })
        .cloned()
        .collect()
}

/// The port a console lease reserved, when it reserved one.
pub fn console_port(lease: &crate::resources::HostResourceLease) -> Option<&str> {
    lease
        .allocations
        .iter()
        .find(|allocation| allocation.key == CONSOLE_PORT_KEY)
        .map(|allocation| allocation.value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{HostLeaseState, HostResourceAllocation, HostResourceLease};

    fn write_config(root: &Path, body: &str) {
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(root.join(".aethyme/config.toml"), body).unwrap();
    }

    fn requirement<'a>(
        request: &'a HostResourceRequest,
        key: &str,
    ) -> Option<&'a HostResourceRequirement> {
        request.resources.iter().find(|r| r.key == key)
    }

    #[test]
    fn a_repository_with_no_console_section_is_singular() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(ConsoleConfig::load(dir.path()), ConsoleConfig::default());
        assert_eq!(ConsoleConfig::default().mode, ConsoleMode::Singular);
    }

    /// An unrelated `[promote]` section must not be read as console policy.
    #[test]
    fn an_unrelated_section_leaves_the_default_intact() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[promote]\nmode = 'manual'\n");
        assert_eq!(ConsoleConfig::load(dir.path()).mode, ConsoleMode::Singular);
    }

    #[test]
    fn the_console_section_is_read() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[console]\nmode = 'per_worktree'\nport = 5000\nport_end = 5010\npool_limit = 7\nttl_seconds = 60\n",
        );
        let config = ConsoleConfig::load(dir.path());
        assert_eq!(config.mode, ConsoleMode::PerWorktree);
        assert_eq!(config.port_start, 5000);
        assert_eq!(config.port_end, 5010);
        assert_eq!(config.pool_limit, 7);
        assert_eq!(config.ttl_seconds, 60);
    }

    /// The CLI spells it `per-worktree` and TOML spells it `per_worktree`;
    /// copying one into the other must not silently select a different mode.
    #[test]
    fn both_spellings_of_per_worktree_parse() {
        assert_eq!(
            ConsoleMode::parse("per-worktree"),
            Some(ConsoleMode::PerWorktree)
        );
        assert_eq!(
            ConsoleMode::parse("per_worktree"),
            Some(ConsoleMode::PerWorktree)
        );
    }

    /// A typo must not silently become a mode with different guarantees.
    #[test]
    fn an_unrecognised_mode_falls_back_rather_than_inventing_one() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[console]\nmode = 'per worktree'\n");
        assert_eq!(ConsoleConfig::load(dir.path()).mode, ConsoleMode::Singular);
    }

    /// A backwards range would otherwise reach the allocator as an empty range
    /// and refuse every console as contention.
    #[test]
    fn a_backwards_port_range_is_clamped_not_left_empty() {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), "[console]\nport = 5000\nport_end = 4000\n");
        let config = ConsoleConfig::load(dir.path());
        assert!(config.port_end >= config.port_start);
    }

    /// The point of `singular` is a URL the operator can bookmark.
    #[test]
    fn singular_pins_one_port_even_when_a_range_is_configured() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[console]\nmode = 'singular'\nport = 4173\nport_end = 4199\n",
        );
        let config = ConsoleConfig::load(dir.path());
        assert_eq!(config.effective_port_range(), (4173, 4173));
    }

    #[test]
    fn per_worktree_allocates_from_the_whole_range() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            "[console]\nmode = 'per_worktree'\nport = 4173\nport_end = 4199\n",
        );
        assert_eq!(
            ConsoleConfig::load(dir.path()).effective_port_range(),
            (4173, 4199)
        );
    }

    /// The exclusive key is the whole mechanism: without it a second launch
    /// would take another port and succeed.
    #[test]
    fn singular_reserves_an_exclusive_key_so_a_second_launch_cannot_start() {
        let config = ConsoleConfig::default();
        let request =
            console_request(&config, "repo-key", Path::new("/w"), "run", None).expect("request");
        match &requirement(&request, CONSOLE_EXCLUSIVE_KEY)
            .expect("exclusive key")
            .resource
        {
            HostResourceKind::ExclusiveKey { name } => assert_eq!(name, "console:repo-key"),
            other => panic!("expected an exclusive key, got {other:?}"),
        }
        match &requirement(&request, CONSOLE_PORT_KEY)
            .expect("port")
            .resource
        {
            HostResourceKind::TcpPort { start, end } => assert_eq!((*start, *end), (4173, 4173)),
            other => panic!("expected a port, got {other:?}"),
        }
    }

    #[test]
    fn per_worktree_reserves_a_port_a_namespace_and_a_bounded_slot() {
        let config = ConsoleConfig {
            mode: ConsoleMode::PerWorktree,
            pool_limit: 3,
            ..ConsoleConfig::default()
        };
        let request =
            console_request(&config, "repo-key", Path::new("/w"), "run", None).expect("request");
        assert!(
            requirement(&request, CONSOLE_EXCLUSIVE_KEY).is_none(),
            "per_worktree must not take the singleton key, or it would serialise the fleet"
        );
        match &requirement(&request, CONSOLE_SLOT_KEY)
            .expect("slot")
            .resource
        {
            HostResourceKind::Capacity { pool, units, limit } => {
                assert_eq!(pool, "console:repo-key");
                assert_eq!((*units, *limit), (1, 3));
            }
            other => panic!("expected capacity, got {other:?}"),
        }
        assert!(requirement(&request, CONSOLE_NAMESPACE_KEY).is_some());
    }

    /// Two worktrees of one repository share the pool and the repository key,
    /// but must not share backing state.
    #[test]
    fn two_worktrees_share_a_pool_but_get_distinct_namespaces() {
        let config = ConsoleConfig {
            mode: ConsoleMode::PerWorktree,
            ..ConsoleConfig::default()
        };
        let first = console_request(&config, "repo", Path::new("/a"), "run", None).unwrap();
        let second = console_request(&config, "repo", Path::new("/b"), "run", None).unwrap();
        assert_eq!(first.repository, second.repository);
        assert_ne!(first.worktree_fingerprint, second.worktree_fingerprint);
        let namespace =
            |request: &HostResourceRequest| match &requirement(request, CONSOLE_NAMESPACE_KEY)
                .unwrap()
                .resource
            {
                HostResourceKind::Namespace { prefix } => prefix.clone(),
                other => panic!("expected a namespace, got {other:?}"),
            };
        assert_ne!(namespace(&first), namespace(&second));
    }

    /// A lease that reserves nothing would appear in the inventory as a
    /// coordinated console when the repository opted out of coordination.
    #[test]
    fn unmanaged_reserves_nothing_at_all() {
        let config = ConsoleConfig {
            mode: ConsoleMode::Unmanaged,
            ..ConsoleConfig::default()
        };
        assert!(console_request(&config, "repo", Path::new("/w"), "run", None).is_none());
    }

    #[test]
    fn the_primary_checkout_is_canonical_and_a_worktree_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        let config = ConsoleConfig::default();
        assert!(console_identity(&config, "repo", &main, &main).canonical);
        assert!(!console_identity(&config, "repo", &main, &worktree).canonical);
    }

    fn lease(repository: &str, keys: &[(&str, &str)], fingerprint: &str) -> HostResourceLease {
        HostResourceLease {
            lease_id: "l".into(),
            request_id: "r".into(),
            repository: repository.into(),
            worktree_fingerprint: fingerprint.into(),
            run_id: "run".into(),
            generation: 1,
            state: HostLeaseState::Active,
            holder_pid: Some(1),
            created_at: 0,
            expires_at: 0,
            released_at: None,
            allocations: keys
                .iter()
                .map(|(key, value)| HostResourceAllocation {
                    key: (*key).into(),
                    kind: "tcp_port".into(),
                    value: (*value).into(),
                    units: None,
                    capacity_limit: None,
                })
                .collect(),
        }
    }

    /// Selection is by reservation, not by naming: a gate lease on the same
    /// repository is not a console.
    #[test]
    fn only_leases_that_reserved_a_console_resource_count_as_consoles() {
        let leases = vec![
            lease("repo", &[(CONSOLE_PORT_KEY, "4173")], "fp-a"),
            lease("repo", &[("managed_cache", "x")], "fp-b"),
            lease("other", &[(CONSOLE_PORT_KEY, "4174")], "fp-c"),
        ];
        let found = console_leases(&leases, "repo");
        assert_eq!(found.len(), 1);
        assert_eq!(console_port(&found[0]), Some("4173"));
    }
}
