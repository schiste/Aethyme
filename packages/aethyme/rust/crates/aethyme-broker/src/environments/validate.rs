//! Static validation of an environments file (#1209). Spawns nothing and
//! opens no host state: the host settings it needs are passed in.
//!
//! Every problem is collected rather than stopping at the first, and every
//! diagnostic names where it is (file, profile, component, field) and how to
//! fix it, because the reader is usually an agent that will act on exactly
//! what the message says.

use std::collections::{BTreeMap, BTreeSet};

use globset::Glob;
use serde::Serialize;

use super::spec::{
    ComponentSpec, ENVIRONMENTS_SCHEMA, Engine, EnvironmentsFile, HealthSpec, HostEnvironmentsFile,
    PoolScope, ProfileSpec, Runtime,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    /// Stable machine code, e.g. `depends_on_cycle`.
    pub code: &'static str,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub message: String,
    pub fix: String,
}

impl Diagnostic {
    /// One line for human output: `file:line:col: [profile/component] field: message (fix)`.
    pub fn render(&self) -> String {
        let mut location = self.file.clone();
        if let (Some(line), Some(column)) = (self.line, self.column) {
            location.push_str(&format!(":{line}:{column}"));
        }
        let mut scope = Vec::new();
        if let Some(profile) = &self.profile {
            scope.push(format!("profile {profile}"));
        }
        if let Some(component) = &self.component {
            scope.push(format!("component {component}"));
        }
        if let Some(field) = &self.field {
            scope.push(format!("field {field}"));
        }
        let scope = if scope.is_empty() {
            String::new()
        } else {
            format!(" [{}]", scope.join(", "))
        };
        format!(
            "{location}:{scope} {} ({}); fix: {}",
            self.message, self.code, self.fix
        )
    }
}

struct Collector<'a> {
    file: &'a str,
    out: Vec<Diagnostic>,
}

impl Collector<'_> {
    fn push(
        &mut self,
        code: &'static str,
        profile: Option<&str>,
        component: Option<&str>,
        field: Option<&str>,
        message: impl Into<String>,
        fix: impl Into<String>,
    ) {
        self.out.push(Diagnostic {
            code,
            file: self.file.to_string(),
            line: None,
            column: None,
            profile: profile.map(str::to_string),
            component: component.map(str::to_string),
            field: field.map(str::to_string),
            message: message.into(),
            fix: fix.into(),
        });
    }
}

/// Budgets must be positive, and a memory budget must name a declared
/// profile (it is measured per profile). Time metric names are free: hooks
/// may report their own.
fn validate_budgets(c: &mut Collector<'_>, file: &EnvironmentsFile) {
    for (metric, seconds) in &file.budgets.seconds {
        if metric.is_empty() || *seconds == 0 {
            c.push(
                "invalid_budget",
                None,
                None,
                Some("budgets.seconds"),
                format!(
                    "budget {metric:?} must be a non-empty name with a positive number of seconds"
                ),
                "e.g. [budgets.seconds] \"up.backend\" = 30",
            );
        }
    }
    for (profile, mib) in &file.budgets.memory_mib {
        if *mib == 0 {
            c.push(
                "invalid_budget",
                Some(profile),
                None,
                Some("budgets.memory_mib"),
                format!("memory budget for {profile:?} must be a positive number of MiB"),
                "e.g. [budgets.memory_mib] backend = 350",
            );
        } else if !file.profiles.iter().any(|p| &p.name == profile) {
            c.push(
                "unknown_profile",
                Some(profile),
                None,
                Some("budgets.memory_mib"),
                format!("memory budget names {profile:?}, which is not a declared profile"),
                "use a profile name from [[profiles]]",
            );
        }
    }
}

/// `[budgets.outcomes]`: at least one window, and a failure rate that is a
/// percentage.
fn validate_budget_outcomes(c: &mut Collector<'_>, file: &EnvironmentsFile) {
    let Some(outcomes) = &file.budgets.outcomes else {
        return;
    };
    if outcomes.windows == 0 {
        c.push(
            "invalid_budget",
            None,
            None,
            Some("budgets.outcomes.windows"),
            "windows must be at least 1",
            "e.g. [budgets.outcomes] windows = 3",
        );
    }
    if outcomes
        .failure_rate_pct
        .is_some_and(|pct| pct == 0 || pct > 100)
    {
        c.push(
            "invalid_budget",
            None,
            None,
            Some("budgets.outcomes.failure_rate_pct"),
            "failure_rate_pct must be between 1 and 100",
            "e.g. [budgets.outcomes] failure_rate_pct = 50",
        );
    }
    if outcomes.labels.iter().any(|l| l.trim().is_empty()) {
        c.push(
            "invalid_budget",
            None,
            None,
            Some("budgets.outcomes.labels"),
            "labels must be non-empty",
            "e.g. [budgets.outcomes] labels = [\"environments\"]",
        );
    }
}

/// `[a-z][a-z0-9_-]{0,31}`: names become hostnames, lease keys and
/// environment-variable suffixes, so they stay in the intersection of all three.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('A'..='Z' | 'a'..='z' | '_'))
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The suffix a component contributes to `AETHYME_ENV_PORT_<SUFFIX>`.
pub fn env_suffix(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Effective rank per profile: explicit `rank`, else declaration order.
pub fn effective_rank(index: usize, profile: &ProfileSpec) -> u32 {
    profile.rank.unwrap_or(index as u32)
}

/// The engine a container component resolves to from the file alone. Host
/// configuration and `AETHYME_CONTAINER_RUNTIME` may still choose at run time.
pub fn declared_engine(file: &EnvironmentsFile, component: &ComponentSpec) -> Option<Engine> {
    component.engine.or(file.engine.default)
}

pub fn validate(
    file: &EnvironmentsFile,
    host: &HostEnvironmentsFile,
    file_label: &str,
    host_label: &str,
) -> Vec<Diagnostic> {
    let mut c = Collector {
        file: file_label,
        out: Vec::new(),
    };
    if file.schema != ENVIRONMENTS_SCHEMA {
        c.push(
            "unsupported_schema",
            None,
            None,
            Some("schema"),
            format!("schema {} is not supported by this binary", file.schema),
            format!("set schema = {ENVIRONMENTS_SCHEMA}, or upgrade aethyme"),
        );
        return c.out;
    }
    if file.ports.start == 0 || file.ports.start > file.ports.end {
        c.push(
            "invalid_port_range",
            None,
            None,
            Some("ports"),
            "ports needs a non-zero inclusive start..end range",
            "e.g. [ports] start = 42000, end = 42999",
        );
    }
    if !valid_route_domain(&file.route_domain) {
        c.push(
            "invalid_route_domain",
            None,
            None,
            Some("route_domain"),
            format!("{:?} is not a hostname", file.route_domain),
            "use lowercase DNS labels separated by dots, e.g. \"localhost\"",
        );
    }
    if file.default_idle_seconds < 60 {
        c.push(
            "idle_too_short",
            None,
            None,
            Some("default_idle_seconds"),
            "idle thresholds under 60 seconds park environments mid-use",
            "set default_idle_seconds >= 60",
        );
    }

    validate_pools(&mut c, file, host, host_label);
    validate_budgets(&mut c, file);
    validate_budget_outcomes(&mut c, file);

    let mut names = BTreeSet::new();
    let mut ranks: BTreeMap<u32, &str> = BTreeMap::new();
    for (index, profile) in file.profiles.iter().enumerate() {
        let name = profile.name.as_str();
        if !valid_name(name) {
            c.push(
                "invalid_name",
                Some(name),
                None,
                Some("name"),
                "profile names must match [a-z][a-z0-9_-]{0,31}",
                "rename the profile",
            );
        }
        if !names.insert(name) {
            c.push(
                "duplicate_profile",
                Some(name),
                None,
                Some("name"),
                "profile declared more than once",
                "give each profile a unique name",
            );
        }
        let rank = effective_rank(index, profile);
        if let Some(other) = ranks.insert(rank, name) {
            c.push(
                "duplicate_rank",
                Some(name),
                None,
                Some("rank"),
                format!("rank {rank} is also used by profile {other}"),
                "ranks form a total order for inference; give every profile a distinct rank",
            );
        }
        validate_profile(&mut c, file, host, profile);
    }
    if file.profiles.is_empty() {
        c.push(
            "no_profiles",
            None,
            None,
            Some("profiles"),
            "no profiles are declared",
            "add at least one [[profiles]] table",
        );
    }
    if let Some(default) = &file.default_profile
        && !names.contains(default.as_str())
    {
        c.push(
            "unknown_profile",
            None,
            None,
            Some("default_profile"),
            format!("default_profile {default:?} is not a declared profile"),
            "name a declared profile",
        );
    }

    let mut rule_names = BTreeSet::new();
    for rule in &file.rules {
        let field = format!("rules.{}", rule.name);
        if !valid_name(&rule.name) || !rule_names.insert(rule.name.as_str()) {
            c.push(
                "invalid_rule_name",
                None,
                None,
                Some(&field),
                "rule names must be unique and match [a-z][a-z0-9_-]{0,31}",
                "rename the rule",
            );
        }
        if !names.contains(rule.min_profile.as_str()) {
            c.push(
                "unknown_profile",
                None,
                None,
                Some(&format!("{field}.min_profile")),
                format!(
                    "min_profile {:?} is not a declared profile",
                    rule.min_profile
                ),
                "name a declared profile",
            );
        }
        if rule.paths.is_empty() {
            c.push(
                "empty_rule",
                None,
                None,
                Some(&format!("{field}.paths")),
                "a rule without paths never fires",
                "add at least one glob",
            );
        }
        for (list, patterns) in [("paths", &rule.paths), ("exclude", &rule.exclude)] {
            for pattern in patterns {
                if let Err(error) = Glob::new(pattern) {
                    c.push(
                        "invalid_glob",
                        None,
                        None,
                        Some(&format!("{field}.{list}")),
                        format!("{pattern:?}: {error}"),
                        "fix the glob syntax",
                    );
                }
            }
        }
    }

    let mut shared_names = BTreeSet::new();
    for shared in &file.shared {
        let field = format!("shared.{}", shared.name);
        if !valid_name(&shared.name) || !shared_names.insert(shared.name.as_str()) {
            c.push(
                "invalid_shared_name",
                None,
                None,
                Some(&field),
                "shared environment names must be unique and match [a-z][a-z0-9_-]{0,31}",
                "rename the shared environment",
            );
        }
        match file.profiles.iter().find(|p| p.name == shared.profile) {
            None => c.push(
                "unknown_profile",
                None,
                None,
                Some(&format!("{field}.profile")),
                format!("profile {:?} is not declared", shared.profile),
                "name a declared profile",
            ),
            Some(profile) if !profile.uses_shared.is_empty() => c.push(
                "shared_uses_shared",
                Some(&profile.name),
                None,
                Some("uses_shared"),
                "a profile used as a shared environment cannot itself use shared environments",
                "remove uses_shared from this profile or use another profile",
            ),
            Some(_) => {}
        }
        if shared.reference.trim().is_empty() {
            c.push(
                "invalid_ref",
                None,
                None,
                Some(&format!("{field}.ref")),
                "ref must not be empty",
                "use \"default_branch\", \"integration\" or refs/heads/<name>",
            );
        }
        if shared.redeploy_on == super::spec::RedeployOn::Inputs && shared.inputs.is_empty() {
            c.push(
                "missing_inputs",
                None,
                None,
                Some(&format!("{field}.inputs")),
                "redeploy_on = \"inputs\" needs the globs whose changes redeploy",
                "add inputs, or use redeploy_on = \"ref\"",
            );
        }
        for pattern in &shared.inputs {
            if let Err(error) = Glob::new(pattern) {
                c.push(
                    "invalid_glob",
                    None,
                    None,
                    Some(&format!("{field}.inputs")),
                    format!("{pattern:?}: {error}"),
                    "fix the glob syntax",
                );
            }
        }
    }
    for profile in &file.profiles {
        for used in &profile.uses_shared {
            if !shared_names.contains(used.as_str()) {
                c.push(
                    "unknown_shared",
                    Some(&profile.name),
                    None,
                    Some("uses_shared"),
                    format!("shared environment {used:?} is not declared"),
                    "declare it in [[shared]] or remove it",
                );
            }
        }
    }
    c.out
}

fn valid_route_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain.len() <= 200
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
}

fn validate_pools(
    c: &mut Collector<'_>,
    file: &EnvironmentsFile,
    host: &HostEnvironmentsFile,
    host_label: &str,
) {
    for (name, pool) in &file.pools {
        let field = format!("pools.{name}");
        if !valid_name(name) {
            c.push(
                "invalid_name",
                None,
                None,
                Some(&field),
                "pool names must match [a-z][a-z0-9_-]{0,31}",
                "rename the pool",
            );
        }
        match (pool.scope, pool.limit) {
            (PoolScope::Repository, None) => c.push(
                "missing_pool_limit",
                None,
                None,
                Some(&format!("{field}.limit")),
                "a repository pool needs a limit",
                "set limit = <units>",
            ),
            (PoolScope::Repository, Some(0)) => c.push(
                "invalid_pool_limit",
                None,
                None,
                Some(&format!("{field}.limit")),
                "limit must be at least 1",
                "set limit >= 1",
            ),
            (PoolScope::Host, Some(_)) => c.push(
                "host_pool_limit_in_repository",
                None,
                None,
                Some(&format!("{field}.limit")),
                "a host pool's limit is set only in host configuration, so repositories cannot disagree about it",
                format!("remove limit here and set [pools.{name}] limit in {host_label}"),
            ),
            (PoolScope::Host, None) => match host.pools.get(name) {
                None => c.push(
                    "missing_host_pool",
                    None,
                    None,
                    Some(&field),
                    format!("host pool {name:?} has no limit in {host_label}"),
                    format!("add [pools.{name}] limit = <units> to {host_label}"),
                ),
                Some(host_pool) if host_pool.limit == 0 => c.push(
                    "invalid_pool_limit",
                    None,
                    None,
                    Some(&field),
                    format!("host pool {name:?} has limit 0 in {host_label}"),
                    "set limit >= 1",
                ),
                Some(_) => {}
            },
            (PoolScope::Repository, Some(_)) => {}
        }
    }
}

/// Resolve a profile's pool name: explicit, or the only declared pool.
pub fn profile_pool<'a>(file: &'a EnvironmentsFile, profile: &'a ProfileSpec) -> Option<&'a str> {
    profile.pool.as_deref().or_else(|| {
        (file.pools.len() == 1)
            .then(|| file.pools.keys().next().map(String::as_str))
            .flatten()
    })
}

/// A pool's effective limit, from the file or host configuration.
pub fn pool_limit(file: &EnvironmentsFile, host: &HostEnvironmentsFile, pool: &str) -> Option<u32> {
    let spec = file.pools.get(pool)?;
    match spec.scope {
        PoolScope::Repository => spec.limit,
        PoolScope::Host => host.pools.get(pool).map(|p| p.limit),
    }
}

fn validate_profile(
    c: &mut Collector<'_>,
    file: &EnvironmentsFile,
    host: &HostEnvironmentsFile,
    profile: &ProfileSpec,
) {
    let p = Some(profile.name.as_str());
    if let Some(idle) = profile.idle_seconds
        && idle < 60
    {
        c.push(
            "idle_too_short",
            p,
            None,
            Some("idle_seconds"),
            "idle thresholds under 60 seconds park environments mid-use",
            "set idle_seconds >= 60",
        );
    }
    if profile.units > 0 {
        match profile_pool(file, profile) {
            None if file.pools.is_empty() => c.push(
                "missing_pool",
                p,
                None,
                Some("pool"),
                "a profile with units needs a pool, and none is declared",
                "declare [pools.<name>] limit = N",
            ),
            None => c.push(
                "missing_pool",
                p,
                None,
                Some("pool"),
                "a profile with units must name its pool when several are declared",
                "set pool = \"<name>\"",
            ),
            Some(pool) if !file.pools.contains_key(pool) => c.push(
                "unknown_pool",
                p,
                None,
                Some("pool"),
                format!("pool {pool:?} is not declared"),
                "declare it under [pools] or name a declared pool",
            ),
            Some(pool) => {
                if let Some(limit) = pool_limit(file, host, pool)
                    && profile.units > limit
                {
                    c.push(
                        "units_exceed_limit",
                        p,
                        None,
                        Some("units"),
                        format!(
                            "units {} exceed pool {pool:?} limit {limit}; this profile could never start",
                            profile.units
                        ),
                        "lower units or raise the pool limit",
                    );
                }
            }
        }
    } else if let Some(pool) = &profile.pool
        && !file.pools.contains_key(pool)
    {
        c.push(
            "unknown_pool",
            p,
            None,
            Some("pool"),
            format!("pool {pool:?} is not declared"),
            "declare it under [pools] or remove pool",
        );
    }
    // A profile with no components is "no environment needed" (#1289):
    // the target of rules for diffs that need nothing.

    let mut names = BTreeSet::new();
    let mut suffixes: BTreeMap<String, &str> = BTreeMap::new();
    let mut listening = 0usize;
    for component in &profile.components {
        let name = component.name.as_str();
        if !valid_name(name) {
            c.push(
                "invalid_name",
                p,
                Some(name),
                Some("name"),
                "component names must match [a-z][a-z0-9_-]{0,31}",
                "rename the component",
            );
        }
        if !names.insert(name) {
            c.push(
                "duplicate_component",
                p,
                Some(name),
                Some("name"),
                "component declared more than once in this profile",
                "give each component a unique name",
            );
        }
        if let Some(other) = suffixes.insert(env_suffix(name), name)
            && other != name
        {
            c.push(
                "component_env_collision",
                p,
                Some(name),
                Some("name"),
                format!(
                    "{name:?} and {other:?} both become AETHYME_ENV_PORT_{}",
                    env_suffix(name)
                ),
                "rename one so the uppercased names differ",
            );
        }
        if component.listen {
            listening += 1;
        }
        validate_component(c, file, profile, component);
    }
    let range = usize::from(file.ports.end.saturating_sub(file.ports.start)) + 1;
    if listening > range {
        c.push(
            "port_range_too_small",
            p,
            None,
            Some("ports"),
            format!("{listening} listening components but the port range holds {range}"),
            "widen [ports]",
        );
    }
    for component in &profile.components {
        for dependency in &component.depends_on {
            if dependency == &component.name {
                c.push(
                    "depends_on_self",
                    p,
                    Some(&component.name),
                    Some("depends_on"),
                    "a component cannot depend on itself",
                    "remove it from depends_on",
                );
            } else if !names.contains(dependency.as_str()) {
                c.push(
                    "unknown_dependency",
                    p,
                    Some(&component.name),
                    Some("depends_on"),
                    format!("{dependency:?} is not a component of this profile"),
                    "name a component of the same profile",
                );
            }
            if !component.optional
                && profile
                    .components
                    .iter()
                    .any(|other| &other.name == dependency && other.optional)
            {
                c.push(
                    "depends_on_optional",
                    p,
                    Some(&component.name),
                    Some("depends_on"),
                    format!("{dependency:?} is optional, so this component could start without it"),
                    "make this component optional too, or the dependency required",
                );
            }
        }
    }
    if let Some(cycle) = find_cycle(&profile.components) {
        c.push(
            "depends_on_cycle",
            p,
            Some(&cycle[0]),
            Some("depends_on"),
            format!("dependency cycle: {}", cycle.join(" -> ")),
            "break the cycle",
        );
    }
}

/// Per-path routes (#1296): only on a routed component; each prefix an
/// absolute path, unique, outside the proxy's control namespace; each target
/// another routed component of the profile, a shared environment the profile
/// uses, or an `http://host[:port]` URL (TLS upstreams are not supported).
fn validate_paths(c: &mut Collector<'_>, profile: &ProfileSpec, component: &ComponentSpec) {
    if component.paths.is_empty() {
        return;
    }
    let p = Some(profile.name.as_str());
    let n = Some(component.name.as_str());
    if !component.routed() {
        c.push(
            "paths_without_route",
            p,
            n,
            Some("paths"),
            "per-path routes need a routed component: they answer on its host",
            "set route = true or remove paths",
        );
    }
    let mut prefixes = BTreeSet::new();
    for (index, path) in component.paths.iter().enumerate() {
        let field = format!("paths[{index}]");
        let prefix = path.prefix.as_str();
        if !prefix.starts_with('/')
            || prefix
                .chars()
                .any(|ch| ch.is_whitespace() || ch == '?' || ch == '#')
            || prefix.starts_with("/__aethyme")
        {
            c.push(
                "invalid_path_prefix",
                p,
                n,
                Some(&format!("{field}.prefix")),
                format!("{prefix:?} is not a usable path prefix"),
                "use an absolute path such as \"/api\", outside /__aethyme",
            );
        } else if !prefixes.insert(prefix.trim_end_matches('/')) {
            c.push(
                "duplicate_path_prefix",
                p,
                n,
                Some(&format!("{field}.prefix")),
                format!("{prefix:?} is routed more than once"),
                "keep one route per prefix",
            );
        }
        let to = path.to.as_str();
        let target_error = if let Some(authority) = to.strip_prefix("http://") {
            let authority = authority.trim_end_matches('/');
            let (host, port) = authority.rsplit_once(':').unwrap_or((authority, "80"));
            (host.is_empty()
                || authority.contains('/')
                || host.contains(['@', ' '])
                || port.parse::<u16>().map_or(true, |port| port == 0))
            .then(|| format!("{to:?} is not an http://host[:port] URL"))
        } else if to.contains("://") {
            Some(format!("{to:?}: only http:// URLs are supported"))
        } else if let Some(shared) = to.strip_prefix("shared:") {
            (!profile.uses_shared.iter().any(|used| used == shared)).then(|| {
                format!("shared environment {shared:?} is not in this profile's uses_shared")
            })
        } else if to == component.name {
            Some("a path cannot route to its own component".to_string())
        } else {
            match profile.components.iter().find(|other| other.name == to) {
                None => Some(format!("{to:?} is not a component of this profile")),
                Some(other) if !other.routed() => Some(format!(
                    "{to:?} is not routed, so it has no host to forward to"
                )),
                Some(_) => None,
            }
        };
        if let Some(message) = target_error {
            c.push(
                "invalid_path_target",
                p,
                n,
                Some(&format!("{field}.to")),
                message,
                "name a routed component, shared:<name> from uses_shared, or http://host[:port]",
            );
        }
    }
}

/// The first cycle found, as a closed path (`a -> b -> a`).
pub fn find_cycle(components: &[ComponentSpec]) -> Option<Vec<String>> {
    let edges: BTreeMap<&str, Vec<&str>> = components
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                c.depends_on
                    .iter()
                    .map(String::as_str)
                    .filter(|d| *d != c.name)
                    .collect(),
            )
        })
        .collect();
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    fn visit<'a>(
        node: &'a str,
        edges: &BTreeMap<&'a str, Vec<&'a str>>,
        marks: &mut BTreeMap<&'a str, Mark>,
        stack: &mut Vec<&'a str>,
    ) -> Option<Vec<String>> {
        match marks.get(node) {
            Some(Mark::Done) => return None,
            Some(Mark::Visiting) => {
                let start = stack.iter().position(|n| *n == node).unwrap_or(0);
                let mut cycle: Vec<String> = stack[start..].iter().map(|n| n.to_string()).collect();
                cycle.push(node.to_string());
                return Some(cycle);
            }
            None => {}
        }
        marks.insert(node, Mark::Visiting);
        stack.push(node);
        for next in edges.get(node).into_iter().flatten() {
            if edges.contains_key(next)
                && let Some(cycle) = visit(next, edges, marks, stack)
            {
                return Some(cycle);
            }
        }
        stack.pop();
        marks.insert(node, Mark::Done);
        None
    }
    let mut marks = BTreeMap::new();
    for node in edges.keys() {
        let mut stack = Vec::new();
        if let Some(cycle) = visit(node, &edges, &mut marks, &mut stack) {
            return Some(cycle);
        }
    }
    None
}

fn validate_component(
    c: &mut Collector<'_>,
    file: &EnvironmentsFile,
    profile: &ProfileSpec,
    component: &ComponentSpec,
) {
    let p = Some(profile.name.as_str());
    let n = Some(component.name.as_str());
    let runtime = component.runtime.as_str();
    let forbid = |c: &mut Collector<'_>, field: &str, present: bool| {
        if present {
            c.push(
                "field_not_supported",
                p,
                n,
                Some(field),
                format!("{field} is not used by the {runtime} runtime"),
                format!("remove {field}"),
            );
        }
    };
    let require = |c: &mut Collector<'_>, field: &str, present: bool| {
        if !present {
            c.push(
                "missing_field",
                p,
                n,
                Some(field),
                format!("the {runtime} runtime needs {field}"),
                format!("set {field}"),
            );
        }
    };
    if component.runtime != Runtime::Static {
        forbid(c, "spa_fallback", component.spa_fallback);
    }
    match component.runtime {
        Runtime::Process => {
            require(c, "start", component.start.is_some());
            forbid(c, "file", component.file.is_some());
            forbid(c, "services", !component.services.is_empty());
            forbid(c, "image", component.image.is_some());
            forbid(c, "args", !component.args.is_empty());
            forbid(c, "container_port", component.container_port.is_some());
            forbid(c, "engine", component.engine.is_some());
            forbid(c, "serve_dir", component.serve_dir.is_some());
        }
        Runtime::Compose => {
            require(c, "file", component.file.is_some());
            forbid(c, "start", component.start.is_some());
            forbid(c, "stop", component.stop.is_some());
            forbid(c, "image", component.image.is_some());
            forbid(c, "args", !component.args.is_empty());
            forbid(c, "container_port", component.container_port.is_some());
            forbid(c, "serve_dir", component.serve_dir.is_some());
            if let Some(engine) = declared_engine(file, component)
                && !engine.supports_compose()
            {
                c.push(
                    "engine_without_compose",
                    p,
                    n,
                    Some("engine"),
                    format!("the {} engine has no compose support", engine.as_str()),
                    "use runtime = \"container\" per service, or engine = \"docker\" / \"podman\"",
                );
            }
        }
        Runtime::Container => {
            require(c, "image", component.image.is_some());
            forbid(c, "start", component.start.is_some());
            forbid(c, "stop", component.stop.is_some());
            forbid(c, "file", component.file.is_some());
            forbid(c, "services", !component.services.is_empty());
            forbid(c, "serve_dir", component.serve_dir.is_some());
            if component.listen {
                require(c, "container_port", component.container_port.is_some());
            } else {
                forbid(c, "container_port", component.container_port.is_some());
            }
        }
        Runtime::Static => {
            require(c, "serve_dir", component.serve_dir.is_some());
            for (field, present) in [
                ("start", component.start.is_some()),
                ("stop", component.stop.is_some()),
                ("file", component.file.is_some()),
                ("services", !component.services.is_empty()),
                ("image", component.image.is_some()),
                ("args", !component.args.is_empty()),
                ("container_port", component.container_port.is_some()),
                ("engine", component.engine.is_some()),
                ("health", component.health.is_some()),
                ("activity_probe", component.activity_probe.is_some()),
            ] {
                forbid(c, field, present);
            }
            if component.listen {
                c.push(
                    "field_not_supported",
                    p,
                    n,
                    Some("listen"),
                    "static components are served by the environment proxy and do not listen",
                    "remove listen",
                );
            }
        }
        Runtime::Hooks => {
            if component.provision.is_none() && component.destroy.is_none() {
                c.push(
                    "missing_field",
                    p,
                    n,
                    Some("provision"),
                    "a hooks component needs provision or destroy",
                    "set provision and destroy",
                );
            }
            if component.provision.is_some() && component.destroy.is_none() {
                c.push(
                    "missing_destroy",
                    p,
                    n,
                    Some("destroy"),
                    "provisioned resources need a destroy command or they leak",
                    "set destroy",
                );
            }
            for (field, present) in [
                ("start", component.start.is_some()),
                ("stop", component.stop.is_some()),
                ("file", component.file.is_some()),
                ("services", !component.services.is_empty()),
                ("image", component.image.is_some()),
                ("args", !component.args.is_empty()),
                ("container_port", component.container_port.is_some()),
                ("engine", component.engine.is_some()),
                ("serve_dir", component.serve_dir.is_some()),
                ("listen", component.listen),
                ("route", component.route == Some(true)),
            ] {
                forbid(c, field, present);
            }
        }
    }
    if component.template.is_some() && component.template_destroy.is_none() {
        c.push(
            "missing_template_destroy",
            p,
            n,
            Some("template_destroy"),
            "templates live outside Aethyme's storage, so eviction needs a command to remove them",
            "set template_destroy",
        );
    }
    if component.template_destroy.is_some() && component.template.is_none() {
        c.push(
            "field_not_supported",
            p,
            n,
            Some("template_destroy"),
            "template_destroy without template",
            "set template or remove template_destroy",
        );
    }
    if component.routed() {
        if component.runtime != Runtime::Static && !component.listen {
            c.push(
                "route_without_listen",
                p,
                n,
                Some("route"),
                "a routed component must listen on a port",
                "set listen = true",
            );
        }
        if component.runtime != Runtime::Static && component.health.is_none() {
            c.push(
                "route_without_health",
                p,
                n,
                Some("health"),
                "a routed component needs a health check so wake knows when to forward",
                "set health = { http = \"/\" } or { tcp = true }",
            );
        }
    }
    validate_paths(c, profile, component);
    if let Some(health) = &component.health {
        validate_health(c, p, n, component, health);
    }
    for key in component.env.keys() {
        if !valid_env_name(key) {
            c.push(
                "invalid_env_name",
                p,
                n,
                Some(&format!("env.{key}")),
                "not a valid environment variable name",
                "use [A-Za-z_][A-Za-z0-9_]*",
            );
        } else if key.starts_with("AETHYME_") {
            c.push(
                "reserved_env_name",
                p,
                n,
                Some(&format!("env.{key}")),
                "AETHYME_* variables are set by Aethyme",
                "rename the variable",
            );
        }
    }
    for name in &component.pass_env {
        if !valid_env_name(name) {
            c.push(
                "invalid_env_name",
                p,
                n,
                Some("pass_env"),
                format!("{name:?} is not a valid environment variable name"),
                "use [A-Za-z_][A-Za-z0-9_]*",
            );
        }
    }
    if !component.enabled_by.is_empty() && !component.optional {
        c.push(
            "enabled_by_without_optional",
            p,
            n,
            Some("enabled_by"),
            "enabled_by only selects optional components",
            "add optional = true, or remove enabled_by",
        );
    }
    for pattern in &component.enabled_by {
        if let Err(error) = Glob::new(pattern) {
            c.push(
                "invalid_glob",
                p,
                n,
                Some("enabled_by"),
                format!("{pattern:?}: {error}"),
                "fix the glob syntax",
            );
        }
    }
    for pattern in component
        .cache_inputs
        .iter()
        .chain(&component.template_inputs)
    {
        if let Err(error) = Glob::new(pattern) {
            c.push(
                "invalid_glob",
                p,
                n,
                Some("cache_inputs"),
                format!("{pattern:?}: {error}"),
                "fix the glob syntax",
            );
        }
    }
    if component.entry_path.is_some()
        && (!component.listen || matches!(component.runtime, Runtime::Static | Runtime::Hooks))
    {
        c.push(
            "field_not_supported",
            p,
            n,
            Some("entry_path"),
            "entry_path requires a listening process, compose or container component",
            "set entry_path on a component with listen = true, or remove it",
        );
    }
    if let Some(entry_path) = component.entry_path.as_deref()
        && (!entry_path.starts_with('/')
            || entry_path
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || c == '#'))
    {
        c.push(
            "invalid_path",
            p,
            n,
            Some("entry_path"),
            format!("entry_path {entry_path:?} must be a URL path starting with `/`"),
            "use a path such as \"/o/\"",
        );
    }
    for (field, value) in [
        ("build_output", component.build_output.as_deref()),
        ("serve_dir", component.serve_dir.as_deref()),
    ] {
        if let Some(value) = value
            && !super::cache::is_worktree_subdirectory(value)
        {
            c.push(
                "invalid_path",
                p,
                n,
                Some(field),
                format!("{field} {value:?} must be a subdirectory of the worktree"),
                "use a relative path without `.` or `..`, e.g. \"dist\"",
            );
        }
    }
    if component.template.is_some() && component.template_inputs.is_empty() {
        c.push(
            "missing_template_inputs",
            p,
            n,
            Some("template_inputs"),
            "a template without template_inputs would never be refreshed",
            "list the paths the template depends on, e.g. [\"db/migrations/**\"]",
        );
    }
    if component.build_output.is_some() && component.build.is_none() {
        c.push(
            "field_not_supported",
            p,
            n,
            Some("build_output"),
            "build_output without build",
            "set build or remove build_output",
        );
    }
    if !component.cache_inputs.is_empty() && component.build.is_none() {
        c.push(
            "field_not_supported",
            p,
            n,
            Some("cache_inputs"),
            "cache_inputs without build",
            "set build or remove cache_inputs",
        );
    }
    if !component.template_inputs.is_empty() && component.template.is_none() {
        c.push(
            "field_not_supported",
            p,
            n,
            Some("template_inputs"),
            "template_inputs without template",
            "set template or remove template_inputs",
        );
    }
}

fn validate_health(
    c: &mut Collector<'_>,
    p: Option<&str>,
    n: Option<&str>,
    component: &ComponentSpec,
    health: &HealthSpec,
) {
    let kinds = usize::from(health.http.is_some())
        + usize::from(health.tcp == Some(true))
        + usize::from(health.command.is_some());
    if kinds != 1 {
        c.push(
            "invalid_health",
            p,
            n,
            Some("health"),
            "health needs exactly one of http, tcp = true, command",
            "keep exactly one probe kind",
        );
    }
    if (health.http.is_some() || health.tcp == Some(true)) && !component.listen {
        c.push(
            "health_without_port",
            p,
            n,
            Some("health"),
            "http and tcp probes need the component to listen",
            "set listen = true or use a command probe",
        );
    }
    if let Some(path) = &health.http
        && !path.starts_with('/')
    {
        c.push(
            "invalid_health",
            p,
            n,
            Some("health.http"),
            "http is a path on the component's own port",
            "use a path such as \"/healthz\"",
        );
    }
    if health.timeout_seconds == 0 || health.timeout_seconds > 3600 {
        c.push(
            "invalid_health",
            p,
            n,
            Some("health.timeout_seconds"),
            "timeout_seconds must be 1..=3600",
            "set a timeout in that range",
        );
    }
    if !(50..=60_000).contains(&health.interval_ms) {
        c.push(
            "invalid_health",
            p,
            n,
            Some("health.interval_ms"),
            "interval_ms must be 50..=60000",
            "set an interval in that range",
        );
    }
}
