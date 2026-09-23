//! Affected gate runner (Phase 4): `.aethyme/gates.toml` config,
//! glob-triggered selection, cheap-first execution with a tree-hash
//! result cache, and cancellation of runs superseded by newer trees.
//!
//! Config format:
//!
//! ```toml
//! [[gate]]
//! name = "cargo-test"
//! command = "cargo test --workspace"
//! cost = 2                     # ascending = cheaper first (default 0)
//! triggers = ["**/*.rs", "Cargo.toml"]   # empty/missing = always runs
//! cache = true                 # false for gates that read commit metadata
//! resource_ttl_seconds = 300
//!
//! [[gate.resources]]
//! key = "database_port"
//! kind = "tcp_port"
//! start = 55000
//! end = 55999
//! ```
//!
//! Selection policy is deliberately over-selecting: a gate with no
//! triggers matches every diff, and an unparseable glob fails config
//! validation rather than silently never matching.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use globset::{Glob, GlobSet, GlobSetBuilder};
use sha2::{Digest, Sha256};

use crate::clock::epoch_ms;
use crate::git::GitRepo;
use crate::store::BrokerStore;
use crate::types::{GateFailureClass, GateStatus, NewGateResult};

pub const GATES_CONFIG_RELPATH: &str = ".aethyme/gates.toml";
pub const GATE_SCOPE_MANIFEST_SCHEMA_VERSION: u32 = 3;

/// Whether a gate run may reuse a conclusive result for the same tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CachePolicy {
    #[default]
    Use,
    Bypass,
}

#[derive(Debug, thiserror::Error)]
pub enum GateConfigError {
    #[error("no gates config at {0} (create it to define gates)")]
    Missing(PathBuf),
    #[error("gates.toml: {0}")]
    Parse(String),
    #[error("gate {gate:?}: invalid trigger glob {glob:?}: {message}")]
    BadGlob {
        gate: String,
        glob: String,
        message: String,
    },
    #[error("gate {gate:?}: invalid host resource profile: {message}")]
    BadResources { gate: String, message: String },
    #[error("gate {gate:?}: invalid timeout_seconds: {message}")]
    BadTimeout { gate: String, message: String },
}

/// One configured gate, with its compiled trigger set.
#[derive(Debug)]
pub struct Gate {
    pub name: String,
    pub command: String,
    pub cost: i64,
    pub triggers: Vec<String>,
    pub cache: bool,
    /// Optional native execution deadline. Absence preserves the historical
    /// unbounded behavior and is surfaced by the advisory gate doctor.
    pub timeout_seconds: Option<u64>,
    pub resources: Vec<crate::HostResourceRequirement>,
    pub resource_ttl_seconds: u64,
    /// Maximum time to wait for a contended host resource bundle. Zero
    /// preserves the historical fail-fast behavior.
    pub resource_wait_seconds: u64,
    pub managed_cache: Option<ManagedGateCache>,
    pub definition_hash: String,
    matcher: Option<GlobSet>,
}

/// Redacted, portable gate definition for selection and routing consumers.
/// The executable command is deliberately excluded; its opaque definition
/// hash still lets consumers detect drift from the broker's execution policy.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GateScopeDefinition {
    pub name: String,
    pub cost: i64,
    pub triggers: Vec<String>,
    pub cache: bool,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    pub resources: Vec<crate::HostResourceRequirement>,
    pub resource_ttl_seconds: u64,
    pub resource_wait_seconds: u64,
    pub managed_cache: Option<ManagedGateCache>,
    pub execution_definition_hash: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticGateScopeContract {
    pub mode: String,
    pub enforced: bool,
    pub frontier_max_depth: usize,
    pub frontier_max_nodes: usize,
    pub result_limit: usize,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GraphIntegrityScopeContract {
    pub authority: crate::GraphAuthority,
    pub enforced: bool,
    pub repository: Option<String>,
    pub policy_sha256: String,
    pub checker_version: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GateScopeManifest {
    pub schema_version: u32,
    pub manifest_sha256: String,
    pub gates: Vec<GateScopeDefinition>,
    pub semantic_advice: SemanticGateScopeContract,
    pub graph_integrity: GraphIntegrityScopeContract,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GateScopeSelection {
    pub gate: String,
    pub triggered_by: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GateScopeEvaluation {
    pub schema_version: u32,
    pub manifest_sha256: String,
    pub base_sha: String,
    pub head_sha: String,
    pub changed_paths: Vec<String>,
    pub selected_gates: Vec<GateScopeSelection>,
    pub graph_integrity: GraphIntegrityScopeContract,
    pub semantic_suggestions_enforced: bool,
    pub semantic_suggestions_included: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum GateScopeError {
    #[error("cannot resolve {role} ref {reference:?} to a commit")]
    MissingRef {
        role: &'static str,
        reference: String,
    },
    #[error("exact head {head_sha} has no {path}")]
    MissingConfiguration { head_sha: String, path: String },
    #[error(
        "gate scope manifest schema {actual} is newer or unsupported; this binary reads schema {supported}"
    )]
    UnsupportedManifestSchema { actual: u32, supported: u32 },
    #[error("gate scope manifest digest does not match its normalized contents")]
    ManifestDigestMismatch,
    #[error(transparent)]
    Config(#[from] GateConfigError),
    #[error(transparent)]
    Git(#[from] crate::GitError),
}

/// A broker-owned, repository-scoped artifact cache used by one gate.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedGateCache {
    /// Stable logical name. It is never interpreted as a filesystem path.
    pub key: String,
    /// Rotate the cache before a run when its stored bytes exceed this bound.
    pub max_bytes: u64,
}

impl Gate {
    /// Whether this gate is triggered by `path`. No triggers = always.
    pub fn matches(&self, path: &str) -> bool {
        match &self.matcher {
            None => true,
            Some(set) => set.is_match(path),
        }
    }
}

/// Load and validate `.aethyme/gates.toml`, sorted cheap-first.
pub fn load_gates(main_root: &Path) -> Result<Vec<Gate>, GateConfigError> {
    let path = main_root.join(GATES_CONFIG_RELPATH);
    let text =
        std::fs::read_to_string(&path).map_err(|_| GateConfigError::Missing(path.clone()))?;
    parse_gates(&text)
}

/// Parse and normalize gate configuration already read from an exact source
/// tree. This is shared by ordinary checkout loading and exact-ref evaluation.
pub fn parse_gates(text: &str) -> Result<Vec<Gate>, GateConfigError> {
    let value: toml::Value = text
        .parse()
        .map_err(|err: toml::de::Error| GateConfigError::Parse(err.to_string()))?;
    let entries = value
        .get("gate")
        .and_then(|gates| gates.as_array())
        .ok_or_else(|| GateConfigError::Parse("expected at least one [[gate]] table".into()))?;

    let mut gates = Vec::new();
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GateConfigError::Parse("gate missing string field 'name'".into()))?
            .to_string();
        let command = entry
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GateConfigError::Parse(format!("gate {name:?} missing string field 'command'"))
            })?
            .to_string();
        let cost = entry.get("cost").and_then(|v| v.as_integer()).unwrap_or(0);
        let cache = entry.get("cache").and_then(|v| v.as_bool()).unwrap_or(true);
        let timeout_seconds = match entry.get("timeout_seconds") {
            None => None,
            Some(value) => {
                let value = value
                    .as_integer()
                    .ok_or_else(|| GateConfigError::BadTimeout {
                        gate: name.clone(),
                        message: "must be a positive integer".into(),
                    })?;
                let value = u64::try_from(value).map_err(|_| GateConfigError::BadTimeout {
                    gate: name.clone(),
                    message: "must be a positive integer".into(),
                })?;
                if value == 0 {
                    return Err(GateConfigError::BadTimeout {
                        gate: name.clone(),
                        message: "must be greater than zero".into(),
                    });
                }
                Some(value)
            }
        };
        let triggers: Vec<String> = entry
            .get("triggers")
            .and_then(|v| v.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let resources: Vec<crate::HostResourceRequirement> = entry
            .get("resources")
            .cloned()
            .map(toml::Value::try_into)
            .transpose()
            .map_err(|error| GateConfigError::BadResources {
                gate: name.clone(),
                message: error.to_string(),
            })?
            .unwrap_or_default();
        let resource_ttl_seconds = entry
            .get("resource_ttl_seconds")
            .and_then(toml::Value::as_integer)
            .map(|value| {
                u64::try_from(value).map_err(|_| GateConfigError::BadResources {
                    gate: name.clone(),
                    message: "resource_ttl_seconds must be positive".into(),
                })
            })
            .transpose()?
            .unwrap_or(300);
        let resource_wait_seconds = entry
            .get("resource_wait_seconds")
            .and_then(toml::Value::as_integer)
            .map(|value| {
                u64::try_from(value).map_err(|_| GateConfigError::BadResources {
                    gate: name.clone(),
                    message: "resource_wait_seconds must be non-negative".into(),
                })
            })
            .transpose()?
            .unwrap_or(0);
        let managed_cache: Option<ManagedGateCache> = entry
            .get("managed_cache")
            .cloned()
            .map(toml::Value::try_into)
            .transpose()
            .map_err(|error| GateConfigError::BadResources {
                gate: name.clone(),
                message: format!("invalid managed_cache: {error}"),
            })?;
        if let Some(cache) = &managed_cache {
            validate_managed_cache(cache).map_err(|message| GateConfigError::BadResources {
                gate: name.clone(),
                message,
            })?;
            if resources
                .iter()
                .any(|resource| resource.key == "managed_cache")
            {
                return Err(GateConfigError::BadResources {
                    gate: name.clone(),
                    message: "resource key 'managed_cache' is reserved by managed_cache".into(),
                });
            }
        }
        crate::validate_host_resource_requirements(&resources, resource_ttl_seconds).map_err(
            |error| GateConfigError::BadResources {
                gate: name.clone(),
                message: error.to_string(),
            },
        )?;
        let definition_hash = gate_definition_hash(
            &name,
            &command,
            cost,
            &triggers,
            cache,
            timeout_seconds,
            &resources,
            resource_ttl_seconds,
            resource_wait_seconds,
            managed_cache.as_ref(),
        );

        let matcher = if triggers.is_empty() {
            None
        } else {
            let mut builder = GlobSetBuilder::new();
            for glob in &triggers {
                builder.add(Glob::new(glob).map_err(|err| GateConfigError::BadGlob {
                    gate: name.clone(),
                    glob: glob.clone(),
                    message: err.to_string(),
                })?);
            }
            Some(builder.build().map_err(|err| GateConfigError::BadGlob {
                gate: name.clone(),
                glob: "<set>".into(),
                message: err.to_string(),
            })?)
        };
        gates.push(Gate {
            name,
            command,
            cost,
            triggers,
            cache,
            timeout_seconds,
            resources,
            resource_ttl_seconds,
            resource_wait_seconds,
            managed_cache,
            definition_hash,
            matcher,
        });
    }
    gates.sort_by(|a, b| a.cost.cmp(&b.cost).then(a.name.cmp(&b.name)));
    Ok(gates)
}

/// Load the selection policy from an exact committed head. This keeps the
/// external evaluator independent of dirty or ignored checkout content.
pub fn load_gates_at_commit(
    repo: &GitRepo,
    head: &str,
) -> Result<(String, Vec<Gate>), GateScopeError> {
    let head_sha = repo
        .resolve_ref(head)
        .ok_or_else(|| GateScopeError::MissingRef {
            role: "head",
            reference: head.into(),
        })?;
    let text = repo
        .file_at_commit(&head_sha, GATES_CONFIG_RELPATH)?
        .ok_or_else(|| GateScopeError::MissingConfiguration {
            head_sha: head_sha.clone(),
            path: GATES_CONFIG_RELPATH.into(),
        })?;
    Ok((head_sha, parse_gates(&text)?))
}

/// Build the content-free portable manifest consumed by external validators.
pub fn gate_scope_manifest(gates: &[Gate]) -> GateScopeManifest {
    gate_scope_manifest_with_graph(gates, &crate::GraphIntegrityPolicy::default())
}

pub fn gate_scope_manifest_with_graph(
    gates: &[Gate],
    graph_policy: &crate::GraphIntegrityPolicy,
) -> GateScopeManifest {
    let definitions = gates
        .iter()
        .map(|gate| GateScopeDefinition {
            name: gate.name.clone(),
            cost: gate.cost,
            triggers: gate.triggers.clone(),
            cache: gate.cache,
            timeout_seconds: gate.timeout_seconds,
            resources: gate.resources.clone(),
            resource_ttl_seconds: gate.resource_ttl_seconds,
            resource_wait_seconds: gate.resource_wait_seconds,
            managed_cache: gate.managed_cache.clone(),
            execution_definition_hash: gate.definition_hash.clone(),
        })
        .collect::<Vec<_>>();
    let semantic_advice = SemanticGateScopeContract {
        mode: "incoming_calls_frontier".into(),
        enforced: false,
        frontier_max_depth: crate::GRAPH_IMPACT_MAX_DEPTH,
        frontier_max_nodes: crate::GRAPH_IMPACT_MAX_NODES,
        result_limit: crate::GRAPH_IMPACT_RESULT_LIMIT,
    };
    let graph_integrity = GraphIntegrityScopeContract {
        authority: graph_policy.authority,
        enforced: graph_policy.enforces_committed_fragments(),
        repository: graph_policy.repository.clone(),
        policy_sha256: graph_policy.digest(),
        checker_version: env!("CARGO_PKG_VERSION").into(),
    };
    let manifest_sha256 =
        gate_scope_manifest_digest(&definitions, &semantic_advice, &graph_integrity);
    GateScopeManifest {
        schema_version: GATE_SCOPE_MANIFEST_SCHEMA_VERSION,
        manifest_sha256,
        gates: definitions,
        semantic_advice,
        graph_integrity,
    }
}

fn gate_scope_manifest_digest(
    definitions: &[GateScopeDefinition],
    semantic_advice: &SemanticGateScopeContract,
    graph_integrity: &GraphIntegrityScopeContract,
) -> String {
    let digest_payload = serde_json::json!({
        "schema_version": GATE_SCOPE_MANIFEST_SCHEMA_VERSION,
        "gates": definitions,
        "semantic_advice": semantic_advice,
        "graph_integrity": graph_integrity,
    });
    format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&digest_payload)
                .expect("gate scope manifest contains only serializable fields")
        )
    )
}

/// Verify a deserialized manifest before an external consumer trusts it.
pub fn verify_gate_scope_manifest(manifest: &GateScopeManifest) -> Result<(), GateScopeError> {
    if manifest.schema_version != GATE_SCOPE_MANIFEST_SCHEMA_VERSION {
        return Err(GateScopeError::UnsupportedManifestSchema {
            actual: manifest.schema_version,
            supported: GATE_SCOPE_MANIFEST_SCHEMA_VERSION,
        });
    }
    let expected = gate_scope_manifest_digest(
        &manifest.gates,
        &manifest.semantic_advice,
        &manifest.graph_integrity,
    );
    if manifest.manifest_sha256 != expected {
        return Err(GateScopeError::ManifestDigestMismatch);
    }
    Ok(())
}

/// Evaluate the same deterministic path selector used by local gate runs for
/// an exact pair of commits. Ref spellings are resolved and never echoed.
pub fn evaluate_gate_scope(
    repo: &GitRepo,
    gates: &[Gate],
    base: &str,
    head: &str,
) -> Result<GateScopeEvaluation, GateScopeError> {
    evaluate_gate_scope_with_graph(
        repo,
        gates,
        &crate::GraphIntegrityPolicy::default(),
        base,
        head,
    )
}

pub fn evaluate_gate_scope_with_graph(
    repo: &GitRepo,
    gates: &[Gate],
    graph_policy: &crate::GraphIntegrityPolicy,
    base: &str,
    head: &str,
) -> Result<GateScopeEvaluation, GateScopeError> {
    let base_sha = repo
        .resolve_ref(base)
        .ok_or_else(|| GateScopeError::MissingRef {
            role: "base",
            reference: base.into(),
        })?;
    let head_sha = repo
        .resolve_ref(head)
        .ok_or_else(|| GateScopeError::MissingRef {
            role: "head",
            reference: head.into(),
        })?;
    let mut changed_paths = repo.gate_scope_changed_between(&base_sha, &head_sha)?;
    changed_paths.sort();
    changed_paths.dedup();
    let selected_gates = select_gates(gates, &changed_paths)
        .into_iter()
        .map(|selection| GateScopeSelection {
            gate: selection.gate.name.clone(),
            reason: if selection.triggered_by.is_some() {
                "path_trigger".into()
            } else {
                "always".into()
            },
            triggered_by: selection.triggered_by,
        })
        .collect();
    let manifest = gate_scope_manifest_with_graph(gates, graph_policy);
    Ok(GateScopeEvaluation {
        schema_version: GATE_SCOPE_MANIFEST_SCHEMA_VERSION,
        manifest_sha256: manifest.manifest_sha256,
        base_sha,
        head_sha,
        changed_paths,
        selected_gates,
        graph_integrity: manifest.graph_integrity,
        semantic_suggestions_enforced: false,
        semantic_suggestions_included: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn gate_definition_hash(
    name: &str,
    command: &str,
    cost: i64,
    triggers: &[String],
    cache: bool,
    timeout_seconds: Option<u64>,
    resources: &[crate::HostResourceRequirement],
    resource_ttl_seconds: u64,
    resource_wait_seconds: u64,
    managed_cache: Option<&ManagedGateCache>,
) -> String {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "name": name,
        "command": command,
        "cost": cost,
        "triggers": triggers,
        "cache": cache,
        "timeout_seconds": timeout_seconds,
        "resources": resources,
        "resource_ttl_seconds": resource_ttl_seconds,
        "resource_wait_seconds": resource_wait_seconds,
        "managed_cache": managed_cache,
    }))
    .expect("gate definition contains only serializable values");
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_managed_cache(cache: &ManagedGateCache) -> Result<(), String> {
    if cache.key.is_empty()
        || cache.key.len() > 64
        || !cache
            .key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || cache.key == "."
        || cache.key == ".."
    {
        return Err("managed_cache.key must be 1-64 ASCII letters, digits, '.', '_' or '-'".into());
    }
    if cache.max_bytes == 0 {
        return Err("managed_cache.max_bytes must be positive".into());
    }
    Ok(())
}

/// Why a gate was selected: the first changed file that triggered it
/// (`None` for always-run gates). Powers `--why`.
#[derive(Debug, serde::Serialize)]
pub struct Selection<'g> {
    #[serde(serialize_with = "gate_name")]
    pub gate: &'g Gate,
    pub triggered_by: Option<String>,
    #[serde(skip)]
    owner_paths: Vec<String>,
}

fn gate_name<S: serde::Serializer>(gate: &&Gate, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&gate.name)
}

/// Deterministic affected-gate selection for a set of changed files.
pub fn select_gates<'g>(gates: &'g [Gate], changed: &[String]) -> Vec<Selection<'g>> {
    let mut selections = Vec::new();
    for gate in gates {
        if gate.matcher.is_none() {
            selections.push(Selection {
                gate,
                triggered_by: None,
                owner_paths: Vec::new(),
            });
            continue;
        }
        let owner_paths = changed
            .iter()
            .filter(|path| gate.matches(path))
            .cloned()
            .collect::<Vec<_>>();
        if let Some(hit) = owner_paths.first() {
            selections.push(Selection {
                gate,
                triggered_by: Some(hit.clone()),
                owner_paths,
            });
        }
    }
    selections
}

/// Outcome of running (or cache-resolving) one gate.
#[derive(Debug, serde::Serialize)]
pub struct GateRunOutcome {
    pub gate: String,
    /// Full Git tree object id proven by this result.
    pub tree_hash: String,
    /// Digest of the command, triggers, cache policy, and resource profile.
    pub definition_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_lease: Option<GateResourceProvenance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_cache: Option<ManagedGateCacheProvenance>,
    pub status: GateStatus,
    pub failure_class: Option<GateFailureClass>,
    pub cached: bool,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    /// Time spent waiting for owner locks, host resources, and cache prep.
    pub wait_duration_ms: Option<i64>,
    /// Time from command spawn until the first stdout/stderr byte appeared.
    pub first_output_ms: Option<i64>,
    /// Combined stdout/stderr bytes captured without exposing their content.
    pub output_bytes: Option<i64>,
    pub log_path: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ManagedGateCacheProvenance {
    pub key: String,
    pub max_bytes: u64,
    pub bytes_before: u64,
    pub bytes_after: Option<u64>,
    pub rotated_before_run: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GateResourceProvenance {
    pub lease_id: String,
    pub generation: u64,
    pub expires_at: i64,
    pub allocations: Vec<crate::HostResourceAllocation>,
}

/// One ref update received from Git's `pre-push` hook protocol.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct PrePushUpdate {
    pub local_ref: String,
    pub local_sha: String,
    pub remote_ref: String,
    pub remote_sha: String,
}

/// Reviewed, read-only interpretation of a `pre-push` hook invocation.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct PrePushPlan {
    pub remote: String,
    pub pushed_sha: Option<String>,
    pub updates: Vec<PrePushUpdate>,
}

/// Result of the opt-in repository-owned pre-push adapter.
#[derive(Debug, serde::Serialize)]
pub struct PrePushReport {
    pub plan: PrePushPlan,
    pub gate_outcomes: Vec<GateRunOutcome>,
}

#[derive(Debug, thiserror::Error)]
pub enum PrePushValidationError {
    #[error(transparent)]
    Git(#[from] crate::GitError),
    #[error(
        "pre-push received no ref updates on stdin; invoke this command from a Git pre-push hook"
    )]
    NoUpdates,
    #[error(
        "invalid pre-push update on line {line}; expected <local-ref> <local-sha> <remote-ref> <remote-sha>"
    )]
    MalformedUpdate { line: usize },
    #[error("pre-push cannot prove multiple different local tips in one checkout: {shas}")]
    MultipleTips { shas: String },
    #[error(
        "pre-push local tip {pushed_sha} is not this checkout's HEAD {head_sha}; run validation from a clean worktree checked out at the pushed tip"
    )]
    TipNotHead {
        pushed_sha: String,
        head_sha: String,
    },
    #[error(
        "pre-push requires a clean checkout so evidence matches the pushed commit; dirty paths: {paths}"
    )]
    DirtyCheckout { paths: String },
}

/// Parse Git's pre-push stdin and prove that a single clean checkout can
/// truthfully validate every non-deletion update. Deletion-only pushes need no
/// content validation and therefore produce a plan without `pushed_sha`.
pub fn plan_pre_push(
    checkout: &GitRepo,
    remote: &str,
    input: &str,
) -> Result<PrePushPlan, PrePushValidationError> {
    let mut updates = Vec::new();
    for (index, line) in input.lines().enumerate() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 4 {
            return Err(PrePushValidationError::MalformedUpdate { line: index + 1 });
        }
        updates.push(PrePushUpdate {
            local_ref: fields[0].to_string(),
            local_sha: fields[1].to_string(),
            remote_ref: fields[2].to_string(),
            remote_sha: fields[3].to_string(),
        });
    }
    if updates.is_empty() {
        return Err(PrePushValidationError::NoUpdates);
    }
    updates.sort_by(|left, right| {
        left.local_ref
            .cmp(&right.local_ref)
            .then(left.remote_ref.cmp(&right.remote_ref))
            .then(left.local_sha.cmp(&right.local_sha))
    });

    let pushed_shas: BTreeSet<_> = updates
        .iter()
        .filter(|update| !update.local_sha.chars().all(|character| character == '0'))
        .map(|update| update.local_sha.clone())
        .collect();
    let pushed_sha = match pushed_shas.len() {
        0 => None,
        1 => pushed_shas.into_iter().next(),
        _ => {
            return Err(PrePushValidationError::MultipleTips {
                shas: pushed_shas.into_iter().collect::<Vec<_>>().join(", "),
            });
        }
    };
    if let Some(pushed_sha) = &pushed_sha {
        let head_sha = checkout.head_commit()?;
        if pushed_sha != &head_sha {
            return Err(PrePushValidationError::TipNotHead {
                pushed_sha: pushed_sha.clone(),
                head_sha,
            });
        }
        let dirty = checkout.dirty_paths()?;
        if !dirty.is_empty() {
            return Err(PrePushValidationError::DirtyCheckout {
                paths: dirty.into_iter().take(10).collect::<Vec<_>>().join(", "),
            });
        }
    }

    Ok(PrePushPlan {
        remote: remote.to_string(),
        pushed_sha,
        updates,
    })
}

/// Sink for human-readable gate progress. Production uses stderr; tests can
/// inject a collector without changing child stdout/stderr capture.
pub trait GateProgressSink: Send + Sync {
    fn report(&self, line: &str);
}

struct StderrGateProgressSink;

impl GateProgressSink for StderrGateProgressSink {
    fn report(&self, line: &str) {
        eprintln!("{line}");
        crate::operations::emit_operation_progress(line);
    }
}

pub(crate) struct GateExecutionContext<'a> {
    pub cache_policy: CachePolicy,
    pub progress: &'a dyn GateProgressSink,
}

fn heartbeat_interval() -> Duration {
    let seconds = std::env::var("AETHYME_GATE_HEARTBEAT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .unwrap_or(30);
    Duration::from_secs(seconds)
}

/// Directory holding pidfiles for in-flight gate runs, enabling
/// cross-process cancellation without a daemon. One file per running
/// gate: `<session>-<gate>.pid` containing
/// `<pgid> <tree_hash> <pid> <start_time>`. Readers need only the first two
/// fields, so older pidfiles (`<pgid> <tree_hash>`) still parse.
fn running_dir(main_root: &Path) -> PathBuf {
    main_root.join(".aethyme/run/gates")
}

/// What a gate pidfile says about the process group it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GatePidRecord {
    pub(crate) pgid: i32,
    pub(crate) tree: String,
    /// The group leader. Gates are spawned as their own group, so this is
    /// `pgid`; absent from pidfiles written before it was recorded.
    pub(crate) pid: Option<i32>,
    /// The leader's start time, in [`process_start_time`] units. It is what
    /// tells the recorded process apart from a later one that reused its PID.
    pub(crate) start: Option<u64>,
}

impl GatePidRecord {
    fn render(&self) -> String {
        let field = |value: Option<String>| value.unwrap_or_else(|| "-".to_string());
        format!(
            "{} {} {} {}",
            self.pgid,
            self.tree,
            field(self.pid.map(|pid| pid.to_string())),
            field(self.start.map(|start| start.to_string())),
        )
    }

    pub(crate) fn parse(content: &str) -> Option<Self> {
        let mut parts = content.split_whitespace();
        let pgid = parts.next()?.parse().ok()?;
        let tree = parts.next()?.to_string();
        let pid = parts.next().and_then(|pid| pid.parse().ok());
        let start = parts.next().and_then(|start| start.parse().ok());
        Some(Self {
            pgid,
            tree,
            pid,
            start,
        })
    }
}

/// Why a recorded gate process group must not be signalled.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SignalRefusal {
    /// 0 is the caller's own group and 1 is init's; a negative value is not
    /// a group at all. None of them is a gate.
    ReservedGroup(i32),
    /// The pidfile predates start-time recording, so the PID's identity
    /// cannot be proven.
    UnrecordedStartTime,
    /// No live process has the recorded PID.
    ProcessGone,
    /// The PID was reused by a different process.
    StartTimeMismatch { recorded: u64, live: u64 },
}

/// The process group to signal for `record`, or why signalling it would be
/// unsafe. `live_start` is the current start time of the recorded leader.
///
/// A crash leaves a pidfile behind, and the kernel reuses PIDs; signalling a
/// stale pgid unchecked can terminate a stranger's process group.
fn signal_target(record: &GatePidRecord, live_start: Option<u64>) -> Result<i32, SignalRefusal> {
    if record.pgid <= 1 {
        return Err(SignalRefusal::ReservedGroup(record.pgid));
    }
    let recorded = record.start.ok_or(SignalRefusal::UnrecordedStartTime)?;
    let live = live_start.ok_or(SignalRefusal::ProcessGone)?;
    if live != recorded {
        return Err(SignalRefusal::StartTimeMismatch { recorded, live });
    }
    Ok(record.pgid)
}

/// When `pid` started, as an opaque value comparable only on this host:
/// microseconds since the epoch on macOS, clock ticks since boot on Linux.
/// `None` when the process does not exist or the platform cannot say.
#[cfg(target_os = "macos")]
fn process_start_time(pid: i32) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: `info` is a writable buffer of exactly `size` bytes, and
    // proc_pidinfo writes at most `size` bytes into it.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: the buffer was zero-initialized and then fully written, and
    // proc_bsdinfo is plain integers and byte arrays, valid for any bits.
    let info = unsafe { info.assume_init() };
    Some(
        info.pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(info.pbi_start_tvusec),
    )
}

#[cfg(target_os = "linux")]
fn process_start_time(pid: i32) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    linux_stat_start_time(&stat)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_start_time(_pid: i32) -> Option<u64> {
    None
}

/// Field 22 (`starttime`) of a `/proc/<pid>/stat` line. The command name in
/// field 2 is parenthesized and may itself contain spaces and parentheses, so
/// fields are counted from the last `)`.
#[cfg(any(target_os = "linux", test))]
fn linux_stat_start_time(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // `rest` begins at field 3, so field 22 is the 20th entry.
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// Write a gate pidfile in one rename, so a reader never sees a torn record.
///
/// Not fsynced, unlike [`crate::atomic_file::with_synced_temporary`]: a
/// pidfile describes processes that do not survive a host crash, so
/// durability buys nothing, and a full sync on the gate start path delays the
/// moment a run becomes cancellable.
fn write_gate_pidfile(path: &Path, record: &GatePidRecord) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("pidfile path has no parent: {}", path.display()),
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".gate-pid-")
        .tempfile_in(parent)?;
    temporary.write_all(record.render().as_bytes())?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Kill in-flight gate runs for `session_id` whose tree differs from
/// `current_tree` (issue #18): they test a superseded state. Records a
/// `cancelled` result for each. Returns the cancelled gate names.
///
/// A result that cannot be recorded is an error, not a silent skip: the run
/// was killed, and without the row nothing says why it has no outcome. The
/// pidfile is removed only after the row lands, so a retry retires it again.
pub fn cancel_obsolete_runs(
    store: &mut BrokerStore,
    main_root: &Path,
    session_id: i64,
    current_tree: &str,
) -> Result<Vec<String>, crate::BrokerError> {
    let dir = running_dir(main_root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let prefix = format!("{session_id}-");
    let mut cancelled = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = file_name.strip_suffix(".pid") else {
            continue;
        };
        let Some(gate_name) = stem.strip_prefix(&prefix) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Some(record) = GatePidRecord::parse(&content) else {
            continue;
        };
        let tree = record.tree.as_str();
        if tree == current_tree {
            continue;
        }
        // Kill the whole process group (the runner spawns each gate in
        // its own group for exactly this purpose). killpg directly:
        // the external `kill` utility on Linux parses "-<pgid>" as an
        // option and silently does nothing. Only a group whose leader is
        // provably the recorded process is signalled; a refusal means the
        // run is already gone (or unprovable), so it is still retired below.
        let live_start = process_start_time(record.pid.unwrap_or(record.pgid));
        if let Ok(pgid) = signal_target(&record, live_start) {
            // SAFETY: killpg takes plain integers and has no memory-safety
            // preconditions; signal_target proved the group is the gate's.
            unsafe {
                libc::killpg(pgid, libc::SIGTERM);
            }
        }
        store.record_gate_result(&NewGateResult {
            gate_name: gate_name.to_string(),
            tree_hash: tree.to_string(),
            definition_hash: String::new(),
            status: GateStatus::Cancelled,
            failure_class: None,
            exit_code: None,
            duration_ms: None,
            wait_duration_ms: None,
            first_output_ms: None,
            output_bytes: None,
            log_path: None,
            session_id: Some(session_id),
        })?;
        // The cancellation is recorded; a pidfile that survives is retired
        // again by the next pass (its process is gone, so nothing is signalled).
        let _ = std::fs::remove_file(entry.path());
        cancelled.push(gate_name.to_string());
    }
    Ok(cancelled)
}

/// Run the affected gates for a checkout, cheap-first, with tree-hash
/// caching. `session_id` scopes cancellation and result attribution.
/// Stops after the first failure (later gates are pointless on a broken
/// tree — and cheaper gates ran first by construction).
pub(crate) fn run_affected(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    changed: &[String],
    session_id: Option<i64>,
    cache_policy: CachePolicy,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    let progress = StderrGateProgressSink;
    run_affected_with_progress(
        store,
        main_root,
        checkout,
        gates,
        changed,
        session_id,
        GateExecutionContext {
            cache_policy,
            progress: &progress,
        },
    )
}

/// Like [`run_affected`], with an injectable progress sink.
pub(crate) fn run_affected_with_progress(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    changed: &[String],
    session_id: Option<i64>,
    context: GateExecutionContext<'_>,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    let selections = select_gates(gates, changed);
    run_selections(
        store,
        main_root,
        checkout,
        selections,
        session_id,
        context.cache_policy,
        context.progress,
    )
}

/// Run EVERY configured gate cheap-first — no diff selection. This is
/// the full-tree "verified" definition shared by CI (`gates run --all`)
/// and the broker: the exact same executor as [`run_affected`], so
/// streaming progress, the tree-hash result cache, and fail-fast
/// semantics are identical by construction.
pub(crate) fn run_all(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    session_id: Option<i64>,
    cache_policy: CachePolicy,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    let progress = StderrGateProgressSink;
    run_all_with_progress(
        store,
        main_root,
        checkout,
        gates,
        session_id,
        cache_policy,
        &progress,
    )
}

/// Like [`run_all`], with an injectable progress sink.
pub(crate) fn run_all_with_progress(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    session_id: Option<i64>,
    cache_policy: CachePolicy,
    progress: &dyn GateProgressSink,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    // Every gate, already cost-sorted by load_gates; `triggered_by` is
    // None because nothing was selected by a diff.
    let selections = gates
        .iter()
        .map(|gate| Selection {
            gate,
            triggered_by: None,
            owner_paths: Vec::new(),
        })
        .collect();
    run_selections(
        store,
        main_root,
        checkout,
        selections,
        session_id,
        cache_policy,
        progress,
    )
}

/// Run one explicitly selected gate. Unlike affected selection, an exact
/// name is authoritative even when its path triggers do not match the diff.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_named(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    gates: &[Gate],
    changed: &[String],
    name: &str,
    session_id: Option<i64>,
    cache_policy: CachePolicy,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    let gate = gates.iter().find(|gate| gate.name == name).ok_or_else(|| {
        crate::broker::BrokerOpError::UnknownGate {
            name: name.to_string(),
        }
    })?;
    let owner_paths = changed
        .iter()
        .filter(|path| gate.matches(path))
        .cloned()
        .collect();
    let progress = StderrGateProgressSink;
    run_selections(
        store,
        main_root,
        checkout,
        vec![Selection {
            gate,
            triggered_by: None,
            owner_paths,
        }],
        session_id,
        cache_policy,
        &progress,
    )
}

struct GateOwnerLocks {
    _locks: Vec<crate::file_lock::ExclusiveFileLock>,
}

impl GateOwnerLocks {
    fn acquire(
        owner_dir: &Path,
        gate_name: &str,
        owner_paths: &[String],
        progress: &dyn GateProgressSink,
    ) -> Result<Self, std::io::Error> {
        std::fs::create_dir_all(owner_dir)?;
        let mut paths = gate_owner_lock_paths(owner_dir, gate_name, owner_paths);
        paths.sort();
        paths.dedup();
        if !paths.is_empty() {
            progress.report(&format!(
                "gate {gate_name} waiting for {} owner lock(s)",
                paths.len()
            ));
        }

        let mut locks = Vec::with_capacity(paths.len());
        for path in paths {
            let file = crate::file_lock::open_lock_file(&path)?;
            locks.push(crate::file_lock::ExclusiveFileLock::acquire(file)?);
        }
        Ok(Self { _locks: locks })
    }
}

fn gate_owner_lock_paths(
    owner_dir: &Path,
    gate_name: &str,
    owner_paths: &[String],
) -> Vec<PathBuf> {
    gate_owner_scope(owner_paths)
        .into_iter()
        .map(|scope| {
            let name = format!(
                "{}-{}-{:016x}.lock",
                lock_segment(gate_name),
                lock_segment(&scope),
                stable_hash(gate_name, &scope)
            );
            owner_dir.join(name)
        })
        .collect()
}

fn gate_owner_scope(owner_paths: &[String]) -> Vec<String> {
    if owner_paths.is_empty() {
        return vec!["all".into()];
    }
    let mut scope = owner_paths.to_vec();
    scope.sort();
    scope.dedup();
    scope
}

fn lock_segment(value: &str) -> String {
    let mut segment = String::new();
    let mut last_was_dash = false;
    for ch in value.chars() {
        let next = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if !last_was_dash {
            Some('-')
        } else {
            None
        };
        if let Some(ch) = next {
            segment.push(ch);
            last_was_dash = ch == '-';
        }
        if segment.len() >= 48 {
            break;
        }
    }
    let trimmed = segment.trim_matches('-');
    if trimmed.is_empty() {
        "scope".into()
    } else {
        trimmed.into()
    }
}

fn stable_hash(gate_name: &str, scope: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in gate_name
        .as_bytes()
        .iter()
        .copied()
        .chain([0])
        .chain(scope.as_bytes().iter().copied())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn gate_worker_id(session_id: Option<i64>, gate_name: &str) -> String {
    let owner = match session_id {
        Some(session_id) => format!("s{session_id}"),
        None => format!("p{}", std::process::id()),
    };
    format!("{}-{}", owner, lock_segment(gate_name))
}

struct GateResourceRuntime {
    grant: crate::HostResourceGrant,
    ttl_seconds: u64,
}

struct ManagedGateCacheRuntime {
    directory: PathBuf,
    provenance: ManagedGateCacheProvenance,
}

impl GateResourceRuntime {
    fn release(&mut self) -> Result<(), crate::HostResourceError> {
        let mut coordinator = crate::HostResourceCoordinator::open_default()?;
        self.grant.lease = coordinator.release(
            &self.grant.lease.lease_id,
            self.grant.lease.generation,
            &self.grant.ownership_token,
        )?;
        Ok(())
    }
}

fn acquire_gate_resources(
    gate: &Gate,
    checkout: &GitRepo,
    tree: &str,
    worker_id: &str,
    progress: &dyn GateProgressSink,
) -> Result<Option<GateResourceRuntime>, String> {
    if gate.resources.is_empty() && gate.managed_cache.is_none() {
        return Ok(None);
    }
    let repository = git_origin_fingerprint(checkout);
    let worktree_fingerprint = sha256_text(&checkout.root().to_string_lossy());
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let request_id = sha256_text(&format!(
        "{repository}:{worktree_fingerprint}:{tree}:{}:{worker_id}:{nonce}",
        std::process::id()
    ));
    let mut resources = gate.resources.clone();
    if let Some(cache) = &gate.managed_cache {
        resources.push(crate::HostResourceRequirement {
            key: "managed_cache".into(),
            resource: crate::HostResourceKind::ExclusiveKey {
                name: format!("aethyme-gate-cache:{repository}:{}", cache.key),
            },
        });
    }
    let request = crate::HostResourceRequest {
        schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
        request_id,
        repository,
        worktree_fingerprint,
        run_id: format!("{}-{}", worker_id, short_tree_hash(tree)),
        ttl_seconds: gate.resource_ttl_seconds,
        holder_pid: Some(std::process::id()),
        resources,
    };
    let mut coordinator =
        crate::HostResourceCoordinator::open_default().map_err(|error| error.to_string())?;
    let mut next_report = std::time::Instant::now();
    let grant = coordinator
        .acquire_with_wait(
            &request,
            std::time::Duration::from_secs(gate.resource_wait_seconds),
            |message| {
                let now = std::time::Instant::now();
                if now >= next_report {
                    progress.report(&format!(
                        "gate {} waiting for host resources: {}",
                        gate.name, message
                    ));
                    next_report = now + std::time::Duration::from_secs(5);
                }
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(Some(GateResourceRuntime {
        grant,
        ttl_seconds: gate.resource_ttl_seconds,
    }))
}

/// Where a repository coordination key came from.
///
/// Both variants are stable across the worktrees of one repository. They
/// differ in what *else* they are stable across: an `origin` key names the
/// same repository from any clone on any machine, while the fallback names
/// one checkout on one machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepositoryKeySource {
    Origin,
    MainCheckoutPath,
}

/// The coordination key for `repo`'s repository, and where it came from.
///
/// #170: resolved from the **main** checkout, never from the calling
/// worktree. Reading it from the caller gave every session its own
/// "repository", which silently turned a gate's `ExclusiveKey` pool into a
/// per-worktree no-op and made every worktree populate its own managed cache
/// from cold. Two configurations took that path:
///
/// - No `origin` at all, which #170 reports.
/// - A *relative* local-path `origin` such as `../upstream`, which
///   `parse_local_path` joins onto the resolving checkout's root. Linked
///   worktrees share the config but not the root, so they disagree for the
///   same reason. An absolute or network `origin` was always worktree-
///   independent and is unaffected.
pub(crate) fn repository_key(repo: &GitRepo) -> (String, RepositoryKeySource) {
    // Degrades to the calling checkout rather than failing: a key an
    // operator can still coordinate under beats a gate that cannot run. The
    // repository has to have become unreadable between discovery and here
    // for this to bind.
    let anchor = repo
        .main_root()
        .ok()
        .and_then(|root| GitRepo::discover(&root).ok());
    let anchor = anchor.as_ref().unwrap_or(repo);
    match anchor.resolve_remote_target("origin", None) {
        Ok(target) => (
            sha256_text(&target.coordination_key),
            RepositoryKeySource::Origin,
        ),
        // The main root's absolute path, not the bare directory name it used
        // to be: two unrelated origin-less repositories both called `app`
        // would otherwise share one key, one managed gate cache and one
        // exclusive pool -- and every non-UTF-8 name collided on the literal
        // `"repository"`. Host resources and managed caches are per-user
        // machine state, so a local path is exactly as durable as the things
        // it keys.
        Err(_) => (
            sha256_text(&anchor.root().to_string_lossy()),
            RepositoryKeySource::MainCheckoutPath,
        ),
    }
}

pub(crate) fn git_origin_fingerprint(repo: &GitRepo) -> String {
    repository_key(repo).0
}

fn prepare_managed_gate_cache(
    policy: Option<&ManagedGateCache>,
    repository: &str,
    progress: &dyn GateProgressSink,
    gate_name: &str,
) -> Result<Option<ManagedGateCacheRuntime>, std::io::Error> {
    if policy.is_none() {
        return Ok(None);
    }
    let root = crate::host_state::default_host_cache_dir().ok_or_else(|| {
        std::io::Error::other("cannot find per-user cache directory; set AETHYME_HOST_CACHE_DIR")
    })?;
    prepare_managed_gate_cache_in(policy, repository, progress, gate_name, &root)
}

fn prepare_managed_gate_cache_in(
    policy: Option<&ManagedGateCache>,
    repository: &str,
    progress: &dyn GateProgressSink,
    gate_name: &str,
    root: &Path,
) -> Result<Option<ManagedGateCacheRuntime>, std::io::Error> {
    let Some(policy) = policy else {
        return Ok(None);
    };
    std::fs::create_dir_all(root)?;
    crate::host_state::protect_host_state_path(root, true)?;
    let repository_root = root.join("gates").join(repository);
    std::fs::create_dir_all(&repository_root)?;
    let directory = repository_root.join(&policy.key);
    let bytes_before = directory_usage(&directory)?;
    let rotated_before_run = bytes_before > policy.max_bytes;
    if rotated_before_run {
        progress.report(&format!(
            "gate {gate_name} rotating managed cache {} ({} bytes exceeds {} bytes)",
            policy.key, bytes_before, policy.max_bytes
        ));
        let retired = repository_root.join(format!(
            ".{}.retired-{}-{}",
            policy.key,
            epoch_ms(),
            std::process::id()
        ));
        std::fs::rename(&directory, &retired)?;
        std::fs::create_dir_all(&directory)?;
        std::fs::remove_dir_all(retired)?;
    } else {
        std::fs::create_dir_all(&directory)?;
    }
    Ok(Some(ManagedGateCacheRuntime {
        directory,
        provenance: ManagedGateCacheProvenance {
            key: policy.key.clone(),
            max_bytes: policy.max_bytes,
            bytes_before,
            bytes_after: None,
            rotated_before_run,
        },
    }))
}

fn directory_usage(path: &Path) -> Result<u64, std::io::Error> {
    if !path.exists() {
        return Ok(0);
    }
    let mut bytes = 0_u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = entry.file_type()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                bytes = bytes.saturating_add(entry.metadata()?.len());
            }
        }
    }
    Ok(bytes)
}

fn sha256_text(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

/// Shared executor for a pre-computed selection: cheap-first order (the
/// selection preserves `load_gates` sorting), tree-hash caching, one
/// process group per gate, fail-fast.
fn run_selections(
    store: &mut BrokerStore,
    main_root: &Path,
    checkout: &GitRepo,
    selections: Vec<Selection<'_>>,
    session_id: Option<i64>,
    cache_policy: CachePolicy,
    progress: &dyn GateProgressSink,
) -> Result<Vec<GateRunOutcome>, crate::broker::BrokerOpError> {
    let tree = checkout.working_tree_hash()?;
    if let Some(session_id) = session_id {
        cancel_obsolete_runs(store, main_root, session_id, &tree)?;
    }

    // A directory that cannot be created fails loudly where the first file
    // inside it is opened (owner locks, pidfile, log), so nothing is lost here.
    let log_dir = main_root.join(".aethyme/logs/gates");
    let _ = std::fs::create_dir_all(&log_dir);
    let run_dir = running_dir(main_root);
    let _ = std::fs::create_dir_all(&run_dir);

    let mut outcomes = Vec::new();
    let mut expensive_advisories_surfaced = false;
    for selection in selections {
        let gate = selection.gate;
        let worker_id = gate_worker_id(session_id, &gate.name);
        if cache_policy == CachePolicy::Bypass {
            crate::warn_unrecorded(
                "record the gate cache bypass event",
                store.append_event(
                    crate::events::GATE_CACHE_BYPASSED,
                    session_id,
                    Some(&crate::events::gate_cache_bypassed_payload(
                        &gate.name, &tree,
                    )),
                ),
            );
        }
        // Cache: conclusive result for this exact tree, any session. The
        // hit is recorded as an event so saved execution time is
        // measurable (kill-criterion accounting). Gates that inspect
        // commit metadata must opt out: the tree can stay identical while
        // commit bodies change.
        if cache_policy == CachePolicy::Use
            && gate.cache
            && let Some(hit) =
                store.cached_gate_result_for_definition(&gate.name, &tree, &gate.definition_hash)?
        {
            let saved_ms = hit.duration_ms.unwrap_or(0);
            progress.report(&format!(
                "gate {} cached ({}, tree {}, saved {}ms)",
                gate.name,
                hit.status.as_str(),
                short_tree_hash(&tree),
                saved_ms
            ));
            crate::warn_unrecorded(
                "record the gate cache hit event",
                store.append_event(
                    crate::events::GATE_CACHED,
                    session_id,
                    Some(&crate::events::gate_cached_payload(
                        &gate.name,
                        &tree,
                        saved_ms,
                        hit.status,
                        cached_failure_class(hit.status),
                    )),
                ),
            );
            let failed = hit.status == GateStatus::Fail;
            outcomes.push(GateRunOutcome {
                gate: gate.name.clone(),
                tree_hash: tree.clone(),
                definition_hash: gate.definition_hash.clone(),
                resource_lease: None,
                managed_cache: None,
                status: hit.status,
                failure_class: cached_failure_class(hit.status),
                cached: true,
                exit_code: hit.exit_code,
                duration_ms: hit.duration_ms,
                wait_duration_ms: Some(0),
                first_output_ms: hit.first_output_ms,
                output_bytes: hit.output_bytes,
                log_path: hit.log_path,
            });
            if failed {
                break;
            }
            continue;
        }

        if gate.cost > 1 && !expensive_advisories_surfaced {
            expensive_advisories_surfaced = true;
            if let Some(session_id) = session_id
                && let Ok(advisories) = store.outstanding_advisories_for_session(session_id)
            {
                crate::warn_unrecorded(
                    "record advisory delivery",
                    store.record_advisories_shown(
                        &advisories,
                        crate::AdvisoryDeliverySurface::PreGate,
                    ),
                );
                for line in crate::advisories::session_notice_lines(&advisories) {
                    progress.report(&line);
                }
            }
        }

        let wait_started = Instant::now();
        let owner_dir = run_dir.join("owners");
        let owner_locks =
            GateOwnerLocks::acquire(&owner_dir, &gate.name, &selection.owner_paths, progress)
                .map_err(|source| crate::BrokerError::Io {
                    path: owner_dir,
                    source,
                })?;
        let log_path = log_dir.join(format!(
            "{}-{}-{}.log",
            gate.name,
            &tree[..8.min(tree.len())],
            worker_id
        ));
        let mut resource_runtime =
            match acquire_gate_resources(gate, checkout, &tree, &worker_id, progress) {
                Ok(runtime) => runtime,
                Err(message) => {
                    let _ = std::fs::write(
                        &log_path,
                        format!("aethyme host resource acquisition failed: {message}\n"),
                    );
                    progress.report(&format!(
                        "gate {} blocked by host resources: {}",
                        gate.name, message
                    ));
                    drop(owner_locks);
                    // An error here is still a failure whose log a later run on
                    // the same tree would overwrite, so it moves aside exactly
                    // as a failing run's log does.
                    let log_path = preserve_failed_gate_log(&log_path, GateStatus::Error);
                    store.record_gate_result(&NewGateResult {
                        gate_name: gate.name.clone(),
                        tree_hash: tree.clone(),
                        definition_hash: gate.definition_hash.clone(),
                        status: GateStatus::Error,
                        failure_class: Some(GateFailureClass::ResourceContention),
                        exit_code: None,
                        duration_ms: Some(0),
                        wait_duration_ms: Some(wait_started.elapsed().as_millis() as i64),
                        first_output_ms: None,
                        output_bytes: Some(0),
                        log_path: Some(log_path.to_string_lossy().into_owned()),
                        session_id,
                    })?;
                    outcomes.push(GateRunOutcome {
                        gate: gate.name.clone(),
                        tree_hash: tree.clone(),
                        definition_hash: gate.definition_hash.clone(),
                        resource_lease: None,
                        managed_cache: None,
                        status: GateStatus::Error,
                        failure_class: Some(GateFailureClass::ResourceContention),
                        cached: false,
                        exit_code: None,
                        duration_ms: Some(0),
                        wait_duration_ms: Some(wait_started.elapsed().as_millis() as i64),
                        first_output_ms: None,
                        output_bytes: Some(0),
                        log_path: Some(log_path.to_string_lossy().into_owned()),
                    });
                    break;
                }
            };
        let resource_provenance = resource_runtime
            .as_ref()
            .map(|runtime| GateResourceProvenance {
                lease_id: runtime.grant.lease.lease_id.clone(),
                generation: runtime.grant.lease.generation,
                expires_at: runtime.grant.lease.expires_at,
                allocations: runtime.grant.lease.allocations.clone(),
            });
        let repository = git_origin_fingerprint(checkout);
        let mut managed_cache_runtime = match prepare_managed_gate_cache(
            gate.managed_cache.as_ref(),
            &repository,
            progress,
            &gate.name,
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                let message = format!("managed cache preparation failed: {error}");
                let _ = std::fs::write(&log_path, format!("aethyme {message}\n"));
                progress.report(&format!("gate {} environment error: {message}", gate.name));
                if let Some(runtime) = resource_runtime.as_mut() {
                    crate::warn_unrecorded(
                        "release the gate's host resource lease",
                        runtime.release(),
                    );
                }
                drop(owner_locks);
                let log_path = preserve_failed_gate_log(&log_path, GateStatus::Error);
                store.record_gate_result(&NewGateResult {
                    gate_name: gate.name.clone(),
                    tree_hash: tree.clone(),
                    definition_hash: gate.definition_hash.clone(),
                    status: GateStatus::Error,
                    failure_class: Some(GateFailureClass::Environment),
                    exit_code: None,
                    duration_ms: Some(0),
                    wait_duration_ms: Some(wait_started.elapsed().as_millis() as i64),
                    first_output_ms: None,
                    output_bytes: Some(0),
                    log_path: Some(log_path.to_string_lossy().into_owned()),
                    session_id,
                })?;
                outcomes.push(GateRunOutcome {
                    gate: gate.name.clone(),
                    tree_hash: tree.clone(),
                    definition_hash: gate.definition_hash.clone(),
                    resource_lease: resource_provenance,
                    managed_cache: None,
                    status: GateStatus::Error,
                    failure_class: Some(GateFailureClass::Environment),
                    cached: false,
                    exit_code: None,
                    duration_ms: Some(0),
                    wait_duration_ms: Some(wait_started.elapsed().as_millis() as i64),
                    first_output_ms: None,
                    output_bytes: Some(0),
                    log_path: Some(log_path.to_string_lossy().into_owned()),
                });
                break;
            }
        };
        let wait_duration_ms = wait_started.elapsed().as_millis() as i64;
        progress.report(&format!(
            "gate {} started (cost {}, tree {})",
            gate.name,
            gate.cost,
            short_tree_hash(&tree)
        ));
        let started = Instant::now();
        let status = run_gate_command(
            &gate.command,
            GateCommandContext {
                cwd: checkout.root(),
                log_path: &log_path,
                run_dir: &run_dir,
                session_id,
                gate_name: &gate.name,
                tree: &tree,
                worker_id: &worker_id,
                owner_paths: &selection.owner_paths,
                timeout_seconds: gate.timeout_seconds,
                started,
                progress,
                resources: resource_runtime.as_ref(),
                managed_cache: managed_cache_runtime.as_ref(),
            },
        );
        if let Some(cache) = managed_cache_runtime.as_mut() {
            cache.provenance.bytes_after = directory_usage(&cache.directory).ok();
        }
        let release_error = resource_runtime
            .as_mut()
            .and_then(|runtime| runtime.release().err())
            .map(|error| format!("host resource release failed: {error}"));
        drop(owner_locks);
        let duration_ms = started.elapsed().as_millis() as i64;
        let status = match (status, release_error) {
            (Ok(mut outcome), release_error) => {
                if outcome.resource_error.is_none() {
                    outcome.resource_error = release_error;
                }
                Ok(outcome)
            }
            // A failed spawn or wait is normally an environment error, but a
            // simultaneous release failure is the more urgent invariant: the
            // host bundle may still be owned and must be reconciled as such.
            (Err(_), Some(release_error)) => Ok(GateCommandOutcome {
                exit_code: None,
                timed_out: false,
                resource_error: Some(release_error),
                first_output_ms: None,
                output_bytes: 0,
            }),
            (Err(error), None) => Err(error),
        };
        let first_output_ms = status
            .as_ref()
            .ok()
            .and_then(|outcome| outcome.first_output_ms);
        let output_bytes = status
            .as_ref()
            .ok()
            .map(|outcome| outcome.output_bytes as i64);
        let (gate_status, failure_class, exit_code) =
            classify_gate_result(&gate.command, &log_path, status);
        // Classification reads the log in place, so the rename waits until
        // after it. A failing log then moves aside: the generic name is keyed
        // on (gate, tree, worker), so a re-run against an unchanged tree lands
        // on the identical path and would erase the failure it just
        // contradicted.
        let log_path = preserve_failed_gate_log(&log_path, gate_status);
        progress.report(&format!(
            "gate {} {} in {}s (tree {})",
            gate.name,
            gate_status.as_str(),
            started.elapsed().as_secs(),
            short_tree_hash(&tree)
        ));
        store.record_gate_result(&NewGateResult {
            gate_name: gate.name.clone(),
            tree_hash: tree.clone(),
            definition_hash: gate.definition_hash.clone(),
            status: gate_status,
            failure_class,
            exit_code,
            duration_ms: Some(duration_ms),
            wait_duration_ms: Some(wait_duration_ms),
            first_output_ms,
            output_bytes,
            log_path: Some(log_path.to_string_lossy().into_owned()),
            session_id,
        })?;
        let failed = gate_status != GateStatus::Pass;
        outcomes.push(GateRunOutcome {
            gate: gate.name.clone(),
            tree_hash: tree.clone(),
            definition_hash: gate.definition_hash.clone(),
            resource_lease: resource_provenance,
            managed_cache: managed_cache_runtime.map(|runtime| runtime.provenance),
            status: gate_status,
            failure_class,
            cached: false,
            exit_code,
            duration_ms: Some(duration_ms),
            wait_duration_ms: Some(wait_duration_ms),
            first_output_ms,
            output_bytes,
            log_path: Some(log_path.to_string_lossy().into_owned()),
        });
        if failed {
            break;
        }
    }
    Ok(outcomes)
}

fn short_tree_hash(tree_hash: &str) -> &str {
    &tree_hash[..12.min(tree_hash.len())]
}

fn classify_gate_result(
    command: &str,
    log_path: &Path,
    status: Result<GateCommandOutcome, std::io::Error>,
) -> (GateStatus, Option<GateFailureClass>, Option<i64>) {
    match status {
        Ok(GateCommandOutcome {
            timed_out: true,
            exit_code,
            resource_error: None,
            ..
        }) => (
            GateStatus::Error,
            Some(GateFailureClass::Timeout),
            exit_code.map(i64::from),
        ),
        Ok(GateCommandOutcome {
            exit_code,
            resource_error: Some(error),
            ..
        }) => {
            let _ = append_gate_log(log_path, &format!("aethyme host resource error: {error}\n"));
            (
                GateStatus::Error,
                Some(GateFailureClass::ResourceContention),
                exit_code.map(i64::from),
            )
        }
        Ok(GateCommandOutcome {
            exit_code: Some(0),
            resource_error: None,
            ..
        }) => (GateStatus::Pass, None, Some(0)),
        Ok(GateCommandOutcome {
            exit_code: Some(code),
            resource_error: None,
            ..
        }) if is_timeout_error(code, log_path) => (
            GateStatus::Error,
            Some(GateFailureClass::Timeout),
            Some(code as i64),
        ),
        Ok(GateCommandOutcome {
            exit_code: Some(code),
            resource_error: None,
            ..
        }) if is_resource_contention_error(command, log_path) => (
            GateStatus::Error,
            Some(GateFailureClass::ResourceContention),
            Some(code as i64),
        ),
        Ok(GateCommandOutcome {
            exit_code: Some(code),
            resource_error: None,
            ..
        }) if is_environment_error(code, log_path) => (
            GateStatus::Error,
            Some(GateFailureClass::Environment),
            Some(code as i64),
        ),
        Ok(GateCommandOutcome {
            exit_code: Some(code),
            resource_error: None,
            ..
        }) if is_build_failure(command, log_path) => (
            GateStatus::Fail,
            Some(GateFailureClass::BuildFailure),
            Some(code as i64),
        ),
        Ok(GateCommandOutcome {
            exit_code: Some(code),
            resource_error: None,
            ..
        }) => (
            GateStatus::Fail,
            Some(GateFailureClass::TestFailure),
            Some(code as i64),
        ),
        // Killed by a signal (cancellation, OOM, operator kill): not a
        // verdict on the code. Recording a conclusive fail here poisons
        // the tree-hash cache — if the same tree recurs, the cached
        // "fail" rejects a submission without ever running the gate.
        Ok(GateCommandOutcome {
            exit_code: None,
            resource_error: None,
            ..
        }) => (GateStatus::Cancelled, None, None),
        // The gate never started, so nothing wrote to the log and the only
        // account of why is `error` itself. Discarding it is what made #167's
        // headroom refusal unreachable: the message was composed, returned,
        // and dropped, leaving an operator with an empty log and a verdict
        // that named no cause. `write_gate_diagnostic` rather than
        // `append_gate_log` because the refusal is raised *before* the log
        // file is created, so an append-only write would lose the same message
        // a second time.
        Err(error) => {
            let class = match error.kind() {
                // Not a different condition from the `resource_error` arm
                // above, just one detected early enough to refuse instead of
                // letting the build discover it as link failures.
                std::io::ErrorKind::StorageFull => GateFailureClass::ResourceContention,
                _ => GateFailureClass::Environment,
            };
            let _ = write_gate_diagnostic(
                log_path,
                &format!("aethyme could not start this gate: {error}\n"),
            );
            (GateStatus::Error, Some(class), None)
        }
    }
}

fn cached_failure_class(status: GateStatus) -> Option<GateFailureClass> {
    match status {
        GateStatus::Fail => Some(GateFailureClass::CachedPriorFail),
        _ => None,
    }
}

fn is_timeout_error(exit_code: i32, log_path: &Path) -> bool {
    if exit_code == 124 {
        return true;
    }
    let Ok(log) = std::fs::read_to_string(log_path) else {
        return false;
    };
    let lower = log.to_ascii_lowercase();
    lower.contains("timed out")
        || lower.contains("timeout exceeded")
        || lower.contains("command timed out")
}

fn is_resource_contention_error(command: &str, log_path: &Path) -> bool {
    // Storage exhaustion is never a verdict on the diff, and it does not depend
    // on which command hit it or where in the tree it surfaced. Checking it
    // before the cargo split is the whole point: a cargo gate whose *test* ran
    // out of space -- not its build -- leaves no `target/` context, so the
    // cargo branch below returned false and the run fell through to
    // `TestFailure`. That records a conclusive `Fail`, which the tree-hash
    // cache then replays on every resubmission of the same tree, so a full disk
    // condemns the very change that would free it (#222).
    if log_contains_any(log_path, &["no space left on device"]) {
        return true;
    }
    if !command_mentions_cargo(command) {
        return log_contains_any(
            log_path,
            &[
                "database is locked",
                "resource busy",
                "resource temporarily unavailable",
                "text file busy",
                "too many open files",
                "no space left on device",
            ],
        );
    }
    let Ok(log) = std::fs::read_to_string(log_path) else {
        return false;
    };
    let lower = log.to_ascii_lowercase();
    let target_context = lower.contains(".fingerprint")
        || lower.contains("target/debug")
        || lower.contains("target/release")
        || lower.contains(".rlib")
        || lower.contains("build directory");
    if !target_context {
        return false;
    }

    const INFRA_PATTERNS: &[&str] = &[
        "extern location",
        "no such file or directory",
        "failed to lock",
        "failed to acquire",
        "failed to rename",
        "failed to remove",
        "failed to write",
        "file exists",
        "resource busy",
        "text file busy",
    ];
    INFRA_PATTERNS.iter().any(|pattern| lower.contains(pattern))
}

fn is_environment_error(exit_code: i32, log_path: &Path) -> bool {
    if exit_code == 126 || exit_code == 127 {
        return true;
    }
    log_contains_any(
        log_path,
        &[
            ": command not found",
            "command not found",
            "not found on path",
            "executable file not found",
            "permission denied",
            "cannot execute",
        ],
    )
}

fn log_contains_any(log_path: &Path, patterns: &[&str]) -> bool {
    let Ok(log) = std::fs::read_to_string(log_path) else {
        return false;
    };
    let lower = log.to_ascii_lowercase();
    patterns.iter().any(|pattern| lower.contains(pattern))
}

/// Whether the gate failed before it could run anything.
///
/// A broken build and a failing assertion are both the diff's fault, but they
/// are different repairs: one fixes a symbol, the other fixes behaviour.
/// Filing both as `TestFailure` hid that -- a stale test import of a renamed
/// symbol was recorded as a test failure, and the only tell was in the
/// numbers: exit 101 after 24s, against the ~400s a run that reaches the tests
/// takes.
///
/// This stays behind the resource and environment checks on purpose. A build
/// that dies because the disk is full is a host problem, and classifying it
/// here would let the tree-hash cache condemn the very change that frees the
/// space.
///
/// Scoped to cargo, whose "could not compile" line is unambiguous and is
/// emitted only when compilation itself failed. Other gates have no portable
/// equivalent, so they keep their existing classification rather than guess.
fn is_build_failure(command: &str, log_path: &Path) -> bool {
    command_mentions_cargo(command) && log_contains_any(log_path, &["could not compile"])
}

fn command_mentions_cargo(command: &str) -> bool {
    command
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-')
        .any(|part| part == "cargo")
}

/// Spawn one gate command (`sh -c`, own process group, output to log),
/// maintaining the pidfile for cancellation. Returns the exit code.
struct GateCommandContext<'a> {
    cwd: &'a Path,
    log_path: &'a Path,
    run_dir: &'a Path,
    session_id: Option<i64>,
    gate_name: &'a str,
    tree: &'a str,
    worker_id: &'a str,
    owner_paths: &'a [String],
    timeout_seconds: Option<u64>,
    started: Instant,
    progress: &'a dyn GateProgressSink,
    resources: Option<&'a GateResourceRuntime>,
    managed_cache: Option<&'a ManagedGateCacheRuntime>,
}

struct GateCommandOutcome {
    exit_code: Option<i32>,
    timed_out: bool,
    resource_error: Option<String>,
    first_output_ms: Option<i64>,
    output_bytes: u64,
}

fn run_gate_command(
    command: &str,
    context: GateCommandContext<'_>,
) -> Result<GateCommandOutcome, std::io::Error> {
    use std::os::unix::process::CommandExt;

    // Before anything is spawned. A build that runs out of space reports link
    // failures and unrelated test failures rather than a disk error, and that
    // verdict is then cached against the tree -- so the retry that would clear
    // it is exactly what the cache prevents. Refusing here keeps the condition
    // and its symptom attached to each other.
    if let Some(refusal) = crate::disk_headroom_refusal(
        crate::available_bytes(context.cwd),
        crate::DEFAULT_GATE_HEADROOM_BYTES,
    ) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            format!("gate {refusal}"),
        ));
    }

    let mut log = std::fs::File::create(context.log_path)?;
    // Before the command's own output, so the head of every gate log says
    // which toolchain produced the verdict below it. A gate that ran against
    // a wrapper is otherwise indistinguishable after the fact from one that
    // ran against the real thing (#177).
    //
    // Through this handle, not a second one opened on the path: the child
    // inherits this descriptor and its offset, and a separate append-mode
    // write would leave that offset at zero for the child to overwrite.
    // `try_clone` below dups it, so stdout and stderr both continue after the
    // note rather than on top of it.
    {
        use std::io::Write as _;
        let _ = log.write_all(crate::git::subprocess_path_note().as_bytes());
    }
    let log_err = log.try_clone()?;
    // Gates execute binaries built from the tree under test.  Those binaries
    // may contain a broker-storage migration that is not present on any
    // reviewed branch yet.  Never let such a child discover the operator's
    // repository database through the normal worktree resolution (#232).
    // Keep the database in a unique temporary directory so the isolation is
    // both per gate and automatically reclaimed when the child exits.
    let isolated_broker_db = tempfile::Builder::new()
        .prefix("broker-db-")
        .tempdir_in(context.run_dir)?;
    let isolated_broker_db_path = isolated_broker_db.path().canonicalize()?.join("broker.db");
    let isolated_broker_scope = crate::gate_database::for_child(
        context.cwd,
        &isolated_broker_db_path,
        std::env::var_os(crate::gate_database::SCOPE_ENV).as_deref(),
    )?;
    let mut process = std::process::Command::new("sh");
    process
        .arg("-c")
        .arg(command)
        .current_dir(context.cwd)
        .env("AETHYME_GATE_WORKER_ID", context.worker_id)
        .env("AETHYME_TEST_DB_SUFFIX", context.worker_id)
        .env(crate::BROKER_DB_ENV, &isolated_broker_db_path)
        .env(crate::gate_database::SCOPE_ENV, isolated_broker_scope)
        .env("AETHYME_GATE_OWNER_PATHS", context.owner_paths.join(":"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(log_err))
        .process_group(0);
    if let Some(resources) = context.resources {
        for (key, value) in resources.grant.environment() {
            process.env(key, value);
        }
    }
    if let Some(cache) = context.managed_cache {
        process.env("AETHYME_GATE_CACHE_DIR", &cache.directory);
    }
    // A gate resolves `git` and `cargo` through PATH, not through the binary
    // this crate proved for itself, so a wrapper the broker routed around
    // still reaches the gate. Left alone when the probe proved nothing to
    // remove.
    if let Some(path) = crate::git::sanitized_subprocess_path() {
        process.env("PATH", path);
    }
    let mut child = process.spawn()?;

    let pidfile = context.session_id.map(|sid| {
        context
            .run_dir
            .join(format!("{sid}-{}.pid", context.gate_name))
    });
    if let Some(pidfile) = &pidfile {
        let pid = child.id() as i32;
        let record = GatePidRecord {
            pgid: pid,
            tree: context.tree.to_string(),
            pid: Some(pid),
            start: process_start_time(pid),
        };
        if let Err(error) = write_gate_pidfile(pidfile, &record) {
            // Without a pidfile the run cannot be cancelled from outside, so
            // it must not run untracked. The child is unreaped, so its PID
            // and group cannot have been reused.
            // SAFETY: killpg takes plain integers and has no memory-safety
            // preconditions.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
            let _ = child.wait();
            return Err(error);
        }
    }
    let fatal_resource_error = std::sync::Arc::new(std::sync::Mutex::new(None));
    let first_output = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(-1));
    let output_monitor_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (status, timed_out) = std::thread::scope(|scope| {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let progress_interval = heartbeat_interval();
        let renewal = context
            .resources
            .map(|runtime| (runtime.grant.clone(), runtime.ttl_seconds));
        let interval = renewal
            .as_ref()
            .map(|(_, ttl)| Duration::from_secs((ttl / 3).max(1)))
            .map(|renewal| renewal.min(progress_interval))
            .unwrap_or(progress_interval);
        let process_group = child.id() as i32;
        let thread_error = fatal_resource_error.clone();
        let monitor_result = first_output.clone();
        let monitor_done = output_monitor_done.clone();
        scope.spawn(move || {
            while !monitor_done.load(std::sync::atomic::Ordering::Acquire) {
                if std::fs::metadata(context.log_path)
                    .map(|metadata| metadata.len() > 0)
                    .unwrap_or(false)
                {
                    let elapsed = context.started.elapsed().as_millis() as i64;
                    let _ = monitor_result.compare_exchange(
                        -1,
                        elapsed,
                        std::sync::atomic::Ordering::AcqRel,
                        std::sync::atomic::Ordering::Acquire,
                    );
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        });
        scope.spawn(move || {
            let mut renewal = renewal;
            let mut last_progress = Instant::now();
            loop {
                match done_rx.recv_timeout(interval) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if let Some((grant, ttl_seconds)) = renewal.as_mut() {
                            match crate::HostResourceCoordinator::open_default().and_then(
                                |mut coordinator| {
                                    coordinator.renew(
                                        &grant.lease.lease_id,
                                        grant.lease.generation,
                                        &grant.ownership_token,
                                        *ttl_seconds,
                                    )
                                },
                            ) {
                                Ok(lease) => grant.lease = lease,
                                Err(error) => {
                                    // Keep retrying while the last confirmed TTL still grants
                                    // authority. Stop the process before that authority expires.
                                    if epoch_ms().saturating_add(1_000) >= grant.lease.expires_at {
                                        if let Ok(mut slot) = thread_error.lock() {
                                            *slot = Some(error.to_string());
                                        }
                                        // SAFETY: killpg has no memory-safety
                                        // preconditions; the leader is unreaped,
                                        // so the group cannot have been reused.
                                        unsafe {
                                            libc::killpg(process_group, libc::SIGTERM);
                                        }
                                        break;
                                    }
                                }
                            }
                        }
                        if last_progress.elapsed() >= progress_interval {
                            context.progress.report(&format!(
                                "gate {} running... {}s",
                                context.gate_name,
                                context.started.elapsed().as_secs()
                            ));
                            last_progress = Instant::now();
                        }
                    }
                }
            }
        });
        let deadline = context.timeout_seconds.map(Duration::from_secs);
        let mut timed_out = false;
        let status = 'wait: loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None)
                    if deadline.is_some_and(|deadline| context.started.elapsed() >= deadline) =>
                {
                    timed_out = true;
                    let seconds = context.timeout_seconds.unwrap_or_default();
                    let _ = append_gate_log(
                        context.log_path,
                        &format!("aethyme gate timeout exceeded after {seconds}s\n"),
                    );
                    // SAFETY: killpg has no memory-safety preconditions; the
                    // leader is unreaped, so the group cannot have been reused.
                    unsafe {
                        libc::killpg(process_group, libc::SIGTERM);
                    }
                    let grace_deadline = Instant::now() + Duration::from_secs(1);
                    let final_status = loop {
                        match child.try_wait() {
                            Ok(Some(status)) => break Ok(status),
                            Ok(None) if Instant::now() >= grace_deadline => {
                                // SAFETY: as for the SIGTERM above.
                                unsafe {
                                    libc::killpg(process_group, libc::SIGKILL);
                                }
                                break child.wait();
                            }
                            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                            Err(error) => break Err(error),
                        }
                    };
                    break 'wait final_status;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(error) => break Err(error),
            }
        };
        output_monitor_done.store(true, std::sync::atomic::Ordering::Release);
        let _ = done_tx.send(());
        (status, timed_out)
    });
    if let Some(pidfile) = &pidfile {
        let _ = std::fs::remove_file(pidfile);
    }
    let resource_error = fatal_resource_error
        .lock()
        .ok()
        .and_then(|mut error| error.take());
    // Killed-by-signal surfaces as no exit code; absent a resource error,
    // `None` remains a non-conclusive cancellation.
    let output_bytes = std::fs::metadata(context.log_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let observed_first_output = first_output.load(std::sync::atomic::Ordering::Acquire);
    Ok(GateCommandOutcome {
        exit_code: status?.code(),
        timed_out,
        resource_error,
        first_output_ms: match observed_first_output {
            -1 if output_bytes > 0 => Some(context.started.elapsed().as_millis() as i64),
            -1 => None,
            elapsed => Some(elapsed),
        },
        output_bytes,
    })
}

fn append_gate_log(path: &Path, message: &str) -> Result<(), std::io::Error> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    file.write_all(message.as_bytes())
}

/// Record why a gate could not start, creating the log if it does not exist.
///
/// [`append_gate_log`] opens for append only, which is correct while a gate is
/// running: the log was created before the spawn, so a missing file there
/// means something else is wrong and should not be papered over. It is wrong
/// for a start failure. `run_gate_command` refuses on disk headroom *before*
/// `File::create`, so the log does not exist yet -- and an append-only write
/// returns `NotFound`, which is how the one message explaining the empty log
/// got discarded twice over (#167, #168).
fn write_gate_diagnostic(path: &Path, message: &str) -> Result<(), std::io::Error> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(message.as_bytes())
}

/// Give a failing gate log a name no later run can reuse.
///
/// Gate logs are named after `(gate, tree, worker)`, which is stable across
/// re-runs of the same tree -- exactly the case where both logs matter. A fail
/// followed by a pass on an unchanged tree is the signature of a flake, and
/// letting the pass overwrite the fail leaves no evidence it ever happened.
/// Passing runs keep the generic name so the directory stays bounded.
///
/// A rename that cannot happen is not worth failing a gate over, so the
/// original path is returned and the result still points at a real file.
fn preserve_failed_gate_log(log_path: &Path, status: GateStatus) -> PathBuf {
    if status == GateStatus::Pass {
        return log_path.to_path_buf();
    }
    let Some(stem) = log_path.file_stem().and_then(|stem| stem.to_str()) else {
        return log_path.to_path_buf();
    };
    let preserved = log_path.with_file_name(format!("{stem}.fail-{}.log", epoch_ms()));
    match std::fs::rename(log_path, &preserved) {
        Ok(()) => preserved,
        Err(_) => log_path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid_record(pgid: i32, start: Option<u64>) -> GatePidRecord {
        GatePidRecord {
            pgid,
            tree: "tree".to_string(),
            pid: Some(pgid),
            start,
        }
    }

    #[test]
    fn a_cancellation_that_cannot_be_recorded_is_an_error_and_keeps_the_pidfile() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("broker.db");
        let mut store = BrokerStore::open(&database).unwrap();
        rusqlite::Connection::open(&database)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER refuse_gate_results BEFORE INSERT ON gate_results
                 BEGIN SELECT RAISE(ABORT, 'simulated gate result write failure'); END;",
            )
            .unwrap();
        let pidfile = running_dir(root.path()).join("7-slow.pid");
        std::fs::create_dir_all(pidfile.parent().unwrap()).unwrap();
        // pgid 1 is never signalled, so no real process group is touched.
        std::fs::write(&pidfile, pid_record(1, None).render()).unwrap();

        let error = cancel_obsolete_runs(&mut store, root.path(), 7, "current tree")
            .expect_err("a cancellation whose result was not recorded must not report success");

        assert!(
            error
                .to_string()
                .contains("simulated gate result write failure"),
            "the store's error must surface: {error}"
        );
        assert!(
            pidfile.exists(),
            "the pidfile must survive so a later pass can record the cancellation"
        );
    }

    #[test]
    fn the_callers_own_group_and_init_are_never_signalled() {
        for pgid in [-1, 0, 1] {
            assert_eq!(
                signal_target(&pid_record(pgid, Some(7)), Some(7)),
                Err(SignalRefusal::ReservedGroup(pgid))
            );
        }
    }

    #[test]
    fn a_reused_pid_is_not_signalled() {
        assert_eq!(
            signal_target(&pid_record(4242, Some(100)), Some(200)),
            Err(SignalRefusal::StartTimeMismatch {
                recorded: 100,
                live: 200
            })
        );
        assert_eq!(
            signal_target(&pid_record(4242, Some(100)), None),
            Err(SignalRefusal::ProcessGone)
        );
    }

    #[test]
    fn a_pidfile_without_a_start_time_is_not_signalled() {
        let legacy = GatePidRecord::parse("4242 abc123").unwrap();
        assert_eq!(legacy.pid, None);
        assert_eq!(legacy.start, None);
        assert_eq!(
            signal_target(&legacy, Some(100)),
            Err(SignalRefusal::UnrecordedStartTime)
        );
    }

    #[test]
    fn the_recorded_process_is_signalled() {
        assert_eq!(
            signal_target(&pid_record(4242, Some(100)), Some(100)),
            Ok(4242)
        );
    }

    #[test]
    fn a_pidfile_round_trips_and_stays_readable_by_two_field_readers() {
        let record = pid_record(4242, Some(99));
        let rendered = record.render();
        assert_eq!(GatePidRecord::parse(&rendered), Some(record));
        let mut legacy_reader = rendered.split_whitespace();
        assert_eq!(legacy_reader.next(), Some("4242"));
        assert_eq!(legacy_reader.next(), Some("tree"));
        let unknown = pid_record(4242, None);
        assert_eq!(GatePidRecord::parse(&unknown.render()), Some(unknown));
    }

    #[test]
    fn the_live_start_time_identifies_this_process() {
        let me = std::process::id() as i32;
        let first = process_start_time(me).expect("this process has a start time");
        assert_eq!(process_start_time(me), Some(first));
        assert_eq!(process_start_time(0), None);
    }

    #[test]
    fn linux_start_time_is_field_22_even_with_spaces_in_the_name() {
        let stat = "123 (a (weird) name) S 1 123 123 0 -1 4194560 100 0 0 0 \
                    5 6 0 0 20 0 1 0 987654 1000 10";
        assert_eq!(linux_stat_start_time(stat), Some(987_654));
        assert_eq!(linux_stat_start_time("garbage"), None);
    }

    #[test]
    fn a_gate_pidfile_is_published_whole_and_leaves_no_temporary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("7-lint.pid");
        let record = pid_record(4242, Some(1));
        write_gate_pidfile(&path, &record).unwrap();
        assert_eq!(
            GatePidRecord::parse(&std::fs::read_to_string(&path).unwrap()),
            Some(record)
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_failing_gate_log_survives_a_later_pass_on_the_same_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join("cargo-test-8b88ffbe-s509-cargo-test.log");

        // The failing run writes the shared name, then moves it aside.
        std::fs::write(&shared, "assertion failed: the evidence that matters\n").unwrap();
        let preserved = preserve_failed_gate_log(&shared, GateStatus::Fail);
        assert_ne!(
            preserved, shared,
            "a failing log must not keep the shared name"
        );
        assert!(
            !shared.exists(),
            "the shared name must be free for the next run"
        );

        // The re-run on the unchanged tree lands on the identical shared path.
        std::fs::write(&shared, "test result: ok. 701 passed\n").unwrap();
        let passing = preserve_failed_gate_log(&shared, GateStatus::Pass);

        assert_eq!(passing, shared, "a passing log keeps the shared name");
        assert_eq!(
            std::fs::read_to_string(&preserved).unwrap(),
            "assertion failed: the evidence that matters\n",
            "the pass overwrote the failure it contradicted"
        );
    }

    #[test]
    fn preserving_a_missing_log_reports_the_original_path() {
        let tmp = tempfile::tempdir().unwrap();
        let absent = tmp.path().join("cargo-test-deadbeef-s1-cargo-test.log");

        // A gate can fail before anything is written. The result still has to
        // name a path rather than lose the row to a rename error.
        assert_eq!(preserve_failed_gate_log(&absent, GateStatus::Fail), absent);
    }

    fn write_config(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir.join(".aethyme")).unwrap();
        std::fs::write(dir.join(GATES_CONFIG_RELPATH), body).unwrap();
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {dir:?}");
    }

    /// A repository with one commit, so `worktree add` has something to
    /// check out. No `origin` unless the caller adds one.
    fn init_repo(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        git_in(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("file.txt"), "fixture\n").unwrap();
        git_in(root, &["add", "-A"]);
        git_in(
            root,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        );
    }

    /// #170: the whole point. A linked worktree is not a different
    /// repository, and a gate resource pool declared to serialise two
    /// workers has to reach both of them.
    #[test]
    fn an_origin_less_repository_keys_every_worktree_the_same() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        init_repo(&root);
        let linked = tmp.path().join("linked");
        git_in(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                linked.to_str().unwrap(),
                "-b",
                "side",
            ],
        );

        let primary = repository_key(&GitRepo::discover(&root).unwrap());
        let worktree = repository_key(&GitRepo::discover(&linked).unwrap());

        assert_eq!(primary.1, RepositoryKeySource::MainCheckoutPath);
        assert_eq!(worktree.1, RepositoryKeySource::MainCheckoutPath);
        assert_eq!(
            primary.0, worktree.0,
            "a linked worktree must coordinate under its repository's key"
        );
    }

    /// The inverse hazard, which the old bare-directory-name material had
    /// and nobody filed: over-sharing. Two unrelated checkouts called `app`
    /// hashed to one key, so they would have shared one managed gate cache.
    #[test]
    fn two_origin_less_repositories_with_the_same_name_do_not_share_a_key() {
        let tmp = tempfile::tempdir().unwrap();
        let left = tmp.path().join("one").join("app");
        let right = tmp.path().join("two").join("app");
        init_repo(&left);
        init_repo(&right);

        let left_key = repository_key(&GitRepo::discover(&left).unwrap());
        let right_key = repository_key(&GitRepo::discover(&right).unwrap());

        assert_eq!(left_key.1, RepositoryKeySource::MainCheckoutPath);
        assert_ne!(
            left_key.0, right_key.0,
            "unrelated repositories sharing a directory name must not share a cache"
        );
    }

    /// A *relative* local-path origin is joined onto the resolving
    /// checkout's root, so it disagreed across worktrees for the same reason
    /// the missing-origin fallback did. #170 reported only the second.
    #[test]
    fn a_relative_local_origin_keys_every_worktree_the_same() {
        let tmp = tempfile::tempdir().unwrap();
        let upstream = tmp.path().join("upstream");
        init_repo(&upstream);
        let root = tmp.path().join("app");
        init_repo(&root);
        git_in(&root, &["remote", "add", "origin", "../upstream"]);
        // Deliberately at a different depth from `app`: a sibling would let
        // `../upstream` resolve to the same absolute path from both roots
        // and the test would pass against the unfixed code.
        let linked = tmp.path().join("nested").join("linked");
        git_in(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                linked.to_str().unwrap(),
                "-b",
                "side",
            ],
        );

        let primary = repository_key(&GitRepo::discover(&root).unwrap());
        let worktree = repository_key(&GitRepo::discover(&linked).unwrap());

        assert_eq!(primary.1, RepositoryKeySource::Origin);
        assert_eq!(worktree.1, RepositoryKeySource::Origin);
        assert_eq!(
            primary.0, worktree.0,
            "one origin is one repository, however the URL is spelled"
        );
    }

    /// An absolute origin was already worktree-independent. Asserted so the
    /// anchoring cannot quietly start deriving the key from a path in the
    /// case that was never broken -- that would rotate every real
    /// repository's gate cache.
    #[test]
    fn an_absolute_origin_still_keys_on_the_remote_and_not_the_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let left = tmp.path().join("here");
        let right = tmp.path().join("elsewhere");
        init_repo(&left);
        init_repo(&right);
        for root in [&left, &right] {
            git_in(
                root,
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://example.invalid/org/app.git",
                ],
            );
        }

        let left_key = repository_key(&GitRepo::discover(&left).unwrap());
        let right_key = repository_key(&GitRepo::discover(&right).unwrap());

        assert_eq!(left_key.1, RepositoryKeySource::Origin);
        assert_eq!(
            left_key.0, right_key.0,
            "two clones of one remote are one repository"
        );
        assert_eq!(
            left_key.0,
            sha256_text("example.invalid/org/app"),
            "sanity: the key is still the origin coordination key"
        );
    }

    /// The `unwrap_or(repo)` anchor fallback. A repository that becomes
    /// unreadable between discovery and keying still yields a key: a gate
    /// coordinating under a degraded key beats a gate that cannot start.
    #[test]
    fn an_unreadable_repository_still_yields_a_key() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("app");
        init_repo(&root);
        let repo = GitRepo::discover(&root).unwrap();
        std::fs::remove_dir_all(&root).unwrap();

        let (key, source) = repository_key(&repo);

        assert_eq!(source, RepositoryKeySource::MainCheckoutPath);
        assert!(!key.is_empty());
    }

    /// A broken build is not a failing test.
    ///
    /// The regression this pins: a test importing a symbol that had been
    /// renamed never compiled, so no assertion ever ran, yet the result was
    /// filed `Fail` / `TestFailure` -- indistinguishable in the record from a
    /// genuine behavioural failure. The repairs are different, and only the
    /// timing hinted at which one it was.
    #[test]
    fn a_build_that_never_compiled_is_not_a_test_failure() {
        let tmp = tempfile::tempdir().unwrap();

        let build_log = tmp.path().join("build.log");
        std::fs::write(
            &build_log,
            "error[E0432]: unresolved import `aethyme_broker::GraphImpactMode`\n             error: could not compile `aethyme-broker` (test \"gates_e2e\") due to 1 previous error\n",
        )
        .unwrap();
        let (status, class, _) = classify_gate_result(
            "cargo test --workspace",
            &build_log,
            Ok(GateCommandOutcome {
                timed_out: false,
                exit_code: Some(101),
                resource_error: None,
                first_output_ms: None,
                output_bytes: 0,
            }),
        );
        assert_eq!(status, GateStatus::Fail, "the diff is still at fault");
        assert_eq!(class, Some(GateFailureClass::BuildFailure));

        // A run that reached the tests and failed one keeps its old class.
        let test_log = tmp.path().join("test.log");
        std::fs::write(
            &test_log,
            "running 1 test\ntest result: FAILED. 0 passed; 1 failed\n",
        )
        .unwrap();
        let (status, class, _) = classify_gate_result(
            "cargo test --workspace",
            &test_log,
            Ok(GateCommandOutcome {
                timed_out: false,
                exit_code: Some(101),
                resource_error: None,
                first_output_ms: None,
                output_bytes: 0,
            }),
        );
        assert_eq!(status, GateStatus::Fail);
        assert_eq!(class, Some(GateFailureClass::TestFailure));
    }

    /// #168: the spawn error was the only account of why the gate produced no
    /// output, and the `Err` arm dropped it on the floor.
    ///
    /// Asserted against the log rather than the returned tuple, because the
    /// tuple was never the bug -- `Error` / `Environment` was already correct.
    /// What an operator got was a verdict with a cause that existed for one
    /// stack frame and was then discarded.
    /// A full disk is not a verdict on the diff, and a cargo gate is where it
    /// is most likely to be mistaken for one.
    ///
    /// The regression this pins: the log of a `cargo test` run whose *test*
    /// exhausted the volume carries no `target/` context, so the cargo-specific
    /// branch of the contention check ignored it and the run was classified
    /// `Fail` / `TestFailure`. A conclusive `Fail` is cached against the tree
    /// hash and replayed, so the tree stayed condemned until the row was
    /// deleted by hand.
    #[test]
    fn a_full_disk_is_contention_whatever_ran_into_it() {
        let tmp = tempfile::tempdir().unwrap();

        // The shape observed in the field: a test, not the build, hit the wall.
        let test_log = tmp.path().join("test.log");
        std::fs::write(
            &test_log,
            "running 6 tests\nredb: I/O error: No space left on device (os error 28)\n",
        )
        .unwrap();
        let (status, class, _) = classify_gate_result(
            "cargo test --workspace",
            &test_log,
            Ok(GateCommandOutcome {
                timed_out: false,
                exit_code: Some(101),
                resource_error: None,
                first_output_ms: None,
                output_bytes: 0,
            }),
        );
        assert_eq!(status, GateStatus::Error, "a full disk is not a verdict");
        assert_eq!(class, Some(GateFailureClass::ResourceContention));
        // An Error is never replayed as a cached verdict, which is what keeps
        // the tree from staying condemned.
        assert_eq!(cached_failure_class(status), None);

        // Still a test failure when the log says nothing about storage.
        let clean_log = tmp.path().join("clean.log");
        std::fs::write(&clean_log, "assertion failed: left == right\n").unwrap();
        let (status, class, _) = classify_gate_result(
            "cargo test --workspace",
            &clean_log,
            Ok(GateCommandOutcome {
                timed_out: false,
                exit_code: Some(101),
                resource_error: None,
                first_output_ms: None,
                output_bytes: 0,
            }),
        );
        assert_eq!(status, GateStatus::Fail);
        assert_eq!(class, Some(GateFailureClass::TestFailure));
    }

    #[test]
    fn a_gate_that_could_not_start_says_why_in_its_log() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("gate.log");
        // Created, so this test is only about the error surviving. Whether the
        // writer can create a missing log is the separate concern that
        // `a_headroom_refusal_reaches_a_log_that_does_not_exist_yet` owns.
        std::fs::write(&log_path, "").unwrap();

        let (status, class, exit) = classify_gate_result(
            "cargo test",
            &log_path,
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "sh: permission denied",
            )),
        );

        assert_eq!(status, GateStatus::Error);
        assert_eq!(class, Some(GateFailureClass::Environment));
        assert_eq!(exit, None);
        let log = std::fs::read_to_string(&log_path).expect("the diagnostic created the log");
        assert!(
            log.contains("sh: permission denied"),
            "the spawn error must survive into the log an operator reads: {log:?}"
        );
    }

    /// #167: the headroom refusal is raised *before* the log file is created,
    /// so an append-only write loses it a second time.
    ///
    /// This is the case that made the refusal unreachable in practice, and it
    /// is why the diagnostic writer creates rather than appends. The
    /// classification travels with it: a full disk is the same resource
    /// condition the running-gate arm reports, just caught early enough to
    /// refuse instead of letting cargo discover it as link failures.
    #[test]
    fn a_headroom_refusal_reaches_a_log_that_does_not_exist_yet() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("never-created.log");

        let (status, class, _) = classify_gate_result(
            "cargo test",
            &log_path,
            Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "gate refusing to start: 1.4 GiB free, 8.0 GiB required",
            )),
        );

        assert_eq!(status, GateStatus::Error);
        assert_eq!(
            class,
            Some(GateFailureClass::ResourceContention),
            "a full disk is a host resource condition, not a property of the code"
        );
        let log = std::fs::read_to_string(&log_path)
            .expect("the refusal must create the log it is the only content of");
        assert!(log.contains("1.4 GiB free"), "{log:?}");
    }

    /// The control: a gate that genuinely ran and failed must keep reporting a
    /// test failure, because that is the one verdict the tree-hash cache
    /// reuses. A fix for #167/#168 that reclassified real failures would make
    /// every gate re-run forever.
    #[test]
    fn a_gate_that_ran_and_failed_is_still_a_test_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("gate.log");
        std::fs::write(&log_path, "test result: FAILED. 1 failed\n").unwrap();

        let (status, class, exit) = classify_gate_result(
            "cargo test",
            &log_path,
            Ok(GateCommandOutcome {
                exit_code: Some(101),
                timed_out: false,
                resource_error: None,
                first_output_ms: Some(12),
                output_bytes: 30,
            }),
        );

        assert_eq!(status, GateStatus::Fail);
        assert_eq!(class, Some(GateFailureClass::TestFailure));
        assert_eq!(exit, Some(101));
    }

    #[test]
    fn config_parses_sorts_cheap_first_and_rejects_bad_globs() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(
            tmp.path(),
            r#"
[[gate]]
name = "pytest"
command = "pytest -q"
cost = 2
triggers = ["**/*.py"]
resource_ttl_seconds = 60
resource_wait_seconds = 15
timeout_seconds = 30

[gate.managed_cache]
key = "python-env"
max_bytes = 1048576

[[gate.resources]]
key = "database_port"
kind = "tcp_port"
start = 55000
end = 55999

[[gate]]
name = "lint"
command = "ruff check ."
cost = 1
triggers = ["**/*.py"]

[[gate]]
name = "always"
command = "true"
"#,
        );
        let gates = load_gates(tmp.path()).unwrap();
        assert_eq!(
            gates.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            vec!["always", "lint", "pytest"]
        );
        let pytest = gates.iter().find(|gate| gate.name == "pytest").unwrap();
        assert_eq!(pytest.resource_ttl_seconds, 60);
        assert_eq!(pytest.resource_wait_seconds, 15);
        assert_eq!(pytest.timeout_seconds, Some(30));
        assert_eq!(
            pytest.managed_cache,
            Some(ManagedGateCache {
                key: "python-env".into(),
                max_bytes: 1_048_576,
            })
        );
        assert_eq!(pytest.resources.len(), 1);
        assert!(matches!(
            pytest.resources[0].resource,
            crate::HostResourceKind::TcpPort {
                start: 55000,
                end: 55999
            }
        ));
        assert_eq!(pytest.definition_hash.len(), 64);

        write_config(
            tmp.path(),
            "[[gate]]\nname = \"bad\"\ncommand = \"x\"\ntriggers = [\"[\"]\n",
        );
        assert!(matches!(
            load_gates(tmp.path()),
            Err(GateConfigError::BadGlob { .. })
        ));

        write_config(
            tmp.path(),
            "[[gate]]\nname='bad-resource'\ncommand='x'\nresource_ttl_seconds=1\n\
             [[gate.resources]]\nkey='slot'\nkind='capacity'\npool='test'\nunits=1\nlimit=1\n",
        );
        assert!(matches!(
            load_gates(tmp.path()),
            Err(GateConfigError::BadResources { .. })
        ));

        for timeout in ["0", "-1", "'soon'"] {
            write_config(
                tmp.path(),
                &format!("[[gate]]\nname='bad-timeout'\ncommand='x'\ntimeout_seconds={timeout}\n"),
            );
            assert!(matches!(
                load_gates(tmp.path()),
                Err(GateConfigError::BadTimeout { .. })
            ));
        }
    }

    #[test]
    fn scope_manifest_is_deterministic_redacted_and_command_drift_bound() {
        let first = parse_gates(
            r#"
[[gate]]
name = "backend"
command = "SECRET_TOKEN=hidden /private/operator/run-tests"
cost = 3
triggers = ["backend/**", "Cargo.toml"]
cache = false
timeout_seconds = 300

[[gate.resources]]
key = "database_port"
kind = "tcp_port"
start = 55000
end = 55999
"#,
        )
        .unwrap();
        let first_manifest = gate_scope_manifest(&first);
        let repeated = gate_scope_manifest(&first);
        assert_eq!(first_manifest, repeated);
        verify_gate_scope_manifest(&first_manifest).unwrap();
        assert_eq!(
            first_manifest.schema_version,
            GATE_SCOPE_MANIFEST_SCHEMA_VERSION
        );
        assert!(!first_manifest.semantic_advice.enforced);
        assert_eq!(first_manifest.gates[0].name, "backend");
        assert_eq!(first_manifest.gates[0].triggers[0], "backend/**");
        assert_eq!(first_manifest.gates[0].timeout_seconds, Some(300));

        let encoded = serde_json::to_string(&first_manifest).unwrap();
        assert!(!encoded.contains("SECRET_TOKEN"), "{encoded}");
        assert!(!encoded.contains("/private/operator"), "{encoded}");
        assert!(!encoded.contains("run-tests"), "{encoded}");

        let second = parse_gates(
            r#"
[[gate]]
name = "backend"
command = "different-command"
cost = 3
triggers = ["backend/**", "Cargo.toml"]
cache = false
timeout_seconds = 301

[[gate.resources]]
key = "database_port"
kind = "tcp_port"
start = 55000
end = 55999
"#,
        )
        .unwrap();
        assert_ne!(
            first_manifest.manifest_sha256,
            gate_scope_manifest(&second).manifest_sha256
        );

        let mut newer = first_manifest.clone();
        newer.schema_version += 1;
        assert!(matches!(
            verify_gate_scope_manifest(&newer),
            Err(GateScopeError::UnsupportedManifestSchema { .. })
        ));
        let mut corrupted = first_manifest.clone();
        corrupted.gates[0].triggers.push("other/**".into());
        assert!(matches!(
            verify_gate_scope_manifest(&corrupted),
            Err(GateScopeError::ManifestDigestMismatch)
        ));
        let mut unknown = serde_json::to_value(&first_manifest).unwrap();
        unknown["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<GateScopeManifest>(unknown).is_err());
    }

    #[test]
    fn managed_cache_rotates_only_its_broker_owned_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let policy = ManagedGateCache {
            key: "cargo".into(),
            max_bytes: 3,
        };
        let directory = tmp.path().join("gates/repository/cargo");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("large"), b"1234").unwrap();

        let runtime = prepare_managed_gate_cache_in(
            Some(&policy),
            "repository",
            &StderrGateProgressSink,
            "test",
            tmp.path(),
        )
        .unwrap()
        .unwrap();

        assert!(runtime.provenance.rotated_before_run);
        assert_eq!(runtime.provenance.bytes_before, 4);
        assert!(runtime.directory.is_dir());
        assert!(!runtime.directory.join("large").exists());
        assert_eq!(
            std::fs::read_dir(tmp.path().join("gates/repository"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn selection_matrix_docs_only_diff_runs_no_test_gates() {
        let tmp = tempfile::tempdir().unwrap();
        write_config(
            tmp.path(),
            r#"
[[gate]]
name = "pytest"
command = "pytest -q"
triggers = ["**/*.py", "pyproject.toml"]

[[gate]]
name = "cargo"
command = "cargo test"
triggers = ["**/*.rs", "**/Cargo.toml"]
"#,
        );
        let gates = load_gates(tmp.path()).unwrap();

        // Docs-only diff → zero gates.
        assert!(select_gates(&gates, &["docs/guide.md".into()]).is_empty());

        // Python diff → pytest only, and --why knows which file.
        let selections = select_gates(
            &gates,
            &[
                "src/auth.py".into(),
                "tests/test_auth.py".into(),
                "README.md".into(),
            ],
        );
        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].gate.name, "pytest");
        assert_eq!(selections[0].triggered_by.as_deref(), Some("src/auth.py"));
        assert_eq!(
            selections[0].owner_paths,
            vec!["src/auth.py".to_string(), "tests/test_auth.py".to_string()]
        );

        // Nested Cargo.toml matches the rooted-glob form.
        let selections = select_gates(&gates, &["crates/x/Cargo.toml".into()]);
        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].gate.name, "cargo");
    }
}
