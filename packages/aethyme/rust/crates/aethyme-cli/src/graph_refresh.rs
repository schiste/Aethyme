//! Version-safe lifecycle for repository-authoritative graph fragments.
//!
//! Planning always regenerates from committed `HEAD` in a disposable clone.
//! It never reads source or graph bytes from the invoking worktree, and its
//! serialized contract contains no absolute paths or source contents.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use aethyme_broker::{Broker, GitRepo, GraphIntegrityStatus, SessionStatus};
use aethyme_engine::store::redb::graph_store::GraphStore;
use aethyme_graph_indexer::{IndexerContext, WalkOptions, index_repo_to_disk, link_repo};
use aethyme_graph_storage::{
    GRAPH_CONFIG_RELPATH, GRAPH_MANIFEST_RELPATH, GraphAuthorityManifest, GraphEntityCounts,
    GraphIntegrityPolicy, GraphLifecycleObservability, GraphStoreArtifactCache, GraphStoreCacheKey,
    committed_source_tree_digest, graph_fragment_set_digest, read_engine_version,
    write_graph_authority_manifest,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const PLAN_SCHEMA_VERSION: u32 = 1;

pub(crate) fn handles(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("status" | "materialize" | "refresh")
    )
}

pub(crate) fn run(args: &[String]) -> u8 {
    match run_inner(args) {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("Error: {message}");
            1
        }
    }
}

fn run_inner(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("status") => {
            let repo = option_path(args, "--repo")?;
            let json = has_flag(args, "--json");
            let status = GraphStatusInspector::inspect(&repo)?;
            render_status(&status, json)
        }
        Some("materialize") => {
            let repo = option_path(args, "--repo")?;
            let report = materialize(&repo)?;
            if has_flag(args, "--json") {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
                );
            } else {
                println!(
                    "Graph store {:?} for {} at {} ({} files, {} ms)",
                    report.action,
                    report.canonical_repository,
                    short(&report.source_head),
                    report.file_count,
                    report.elapsed_ms
                );
                render_performance_text(&report.performance);
                render_cache_text(&report.cache);
            }
            Ok(())
        }
        Some("refresh") => match args.get(1).map(String::as_str) {
            Some("plan") => {
                let repo = option_path(args, "--repo")?;
                let json = has_flag(args, "--json");
                let diff = has_flag(args, "--diff");
                if json && diff {
                    return Err("--json and --diff are separate review surfaces".into());
                }
                let built = build_plan(&repo)?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&built.plan).map_err(|e| e.to_string())?
                    );
                } else {
                    render_plan_text(&built.plan);
                }
                if diff {
                    print!("{}", render_change_summary(&built.plan));
                }
                Ok(())
            }
            Some("execute") => {
                let repo = option_path(args, "--repo")?;
                let confirmation = required_option(args, "--confirm")?;
                let plan = execute(&repo, confirmation)?;
                if has_flag(args, "--json") {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&plan).map_err(|error| error.to_string())?
                    );
                } else {
                    println!(
                        "Graph refresh complete for {} at {} ({} fragment change(s)); derived store {:?}",
                        plan.canonical_repository,
                        short(&plan.source.head_sha),
                        plan.changes.len(),
                        plan.derived_store.action
                    );
                    render_performance_text(&plan.performance);
                    render_cache_text(&plan.cache);
                }
                Ok(())
            }
            Some("recover") => {
                let repo = option_path(args, "--repo")?;
                let digest = required_option(args, "--plan")?;
                recover(&repo, digest)?;
                println!("Graph refresh recovery completed for plan {digest}");
                Ok(())
            }
            _ => Err(
                "usage: aethyme graph refresh <plan|execute|recover> --repo <path> [--json|--diff]"
                    .into(),
            ),
        },
        _ => Err("usage: aethyme graph <status|materialize|refresh> --repo <path>".into()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GraphMaterializationReport {
    schema_version: u32,
    canonical_repository: String,
    source_head: String,
    fragment_status: GraphIntegrityStatus,
    action: DerivedStoreAction,
    file_count: usize,
    elapsed_ms: u128,
    work: GraphLifecycleWork,
    performance: GraphLifecycleObservability,
    cache: GraphMaterializationCache,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GraphMaterializationCache {
    status: GraphMaterializationCacheStatus,
    key_sha256: Option<String>,
    artifact_bytes: u64,
}

impl Default for GraphMaterializationCache {
    fn default() -> Self {
        Self {
            status: GraphMaterializationCacheStatus::NotNeeded,
            key_sha256: None,
            artifact_bytes: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GraphMaterializationCacheStatus {
    NotNeeded,
    Unavailable,
    Hit,
    MissStored,
    MissUnstored,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
struct GraphLifecycleWork {
    disposable_clones: u32,
    source_index_runs: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GraphStatusReport {
    schema_version: u32,
    canonical_repository: String,
    source: GraphSourceState,
    policy: GraphPolicyState,
    installed: InstalledGraphRuntime,
    fragments: FragmentState,
    derived_store: DerivedStoreState,
    compatibility: GraphRefreshCompatibility,
    healthy: bool,
    action_required: bool,
    safe_to_refresh: bool,
    refresh_plan_required: bool,
    blockers: Vec<String>,
    diagnosis: Option<String>,
    next_action: String,
    work: GraphLifecycleWork,
    performance: GraphLifecycleObservability,
}

struct GraphStatusInspector;

struct GraphMaterializationPlan {
    canonical_repository: String,
    source_head: String,
    fragment_status: GraphIntegrityStatus,
    action: DerivedStoreAction,
    committed_files: BTreeMap<String, GraphFileBytes>,
    work: GraphLifecycleWork,
    performance: GraphLifecycleObservability,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GraphRefreshPlan {
    schema_version: u32,
    plan_sha256: String,
    canonical_repository: String,
    source: GraphSourceState,
    policy: GraphPolicyState,
    installed: InstalledGraphRuntime,
    fragments: FragmentState,
    derived_store: DerivedStoreState,
    changes: Vec<GraphFileChange>,
    dirty_paths: Vec<String>,
    applied_uncommitted_paths: Vec<String>,
    overlapping_dirty_paths: Vec<String>,
    disjoint_dirty_paths: Vec<String>,
    active_sessions: Vec<GraphSessionPrecondition>,
    relevant_leases: Vec<GraphLeasePrecondition>,
    compatibility: GraphRefreshCompatibility,
    safe_to_execute: bool,
    blockers: Vec<String>,
    next_action: String,
    work: GraphLifecycleWork,
    performance: GraphLifecycleObservability,
    cache: GraphMaterializationCache,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GraphSourceState {
    head_sha: String,
    tree_sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GraphPolicyState {
    authority: aethyme_broker::GraphAuthority,
    repository: Option<String>,
    policy_sha256: String,
    pinned_engine_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct InstalledGraphRuntime {
    router_version: String,
    linked_engine_version: String,
    graph_indexer_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FragmentState {
    status: GraphIntegrityStatus,
    existing_file_count: usize,
    proposed_file_count: usize,
    changed_file_count: usize,
    working_tree_matches_proposal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DerivedStoreState {
    status: DerivedStoreStatus,
    indexed_commit: Option<String>,
    action: DerivedStoreAction,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DerivedStoreStatus {
    Disabled,
    Missing,
    Current,
    Stale,
    Unreadable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DerivedStoreAction {
    None,
    MaterializeAfterFragmentVerification,
    ReplaceAfterFragmentVerification,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GraphFileAction {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GraphFileChange {
    path: String,
    action: GraphFileAction,
    before_sha256: Option<String>,
    after_sha256: Option<String>,
    before_mode: Option<String>,
    after_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GraphSessionPrecondition {
    session_id: i64,
    status: SessionStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct GraphLeasePrecondition {
    session_id: i64,
    path: String,
    kind: aethyme_broker::LeaseKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GraphRefreshCompatibility {
    Compatible,
    AuthorityDisabled,
    MissingEnginePin,
    VersionMismatch,
}

struct BuiltPlan {
    plan: GraphRefreshPlan,
    existing_files: BTreeMap<String, GraphFileBytes>,
    proposed_files: BTreeMap<String, GraphFileBytes>,
}

struct ProposedRepository {
    _temporary: tempfile::TempDir,
    root: PathBuf,
}

impl ProposedRepository {
    fn from_committed_head(
        repo: &Path,
        head: &str,
        work: &mut GraphLifecycleWork,
        performance: &mut GraphLifecycleObservability,
    ) -> Result<Self, String> {
        let started = std::time::Instant::now();
        work.disposable_clones += 1;
        let temporary = tempfile::Builder::new()
            .prefix("aethyme-graph-refresh-plan-")
            .tempdir()
            .map_err(|error| format!("create disposable graph plan directory: {error}"))?;
        let root = temporary.path().join("repository");
        let output = Command::new("git")
            .arg("clone")
            .args(["--quiet", "--no-checkout", "--no-hardlinks"])
            .arg(repo)
            .arg(&root)
            .output()
            .map_err(|error| format!("materialize committed graph source: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "materialize committed graph source: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        git(
            &root,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "checkout",
                "--quiet",
                "--detach",
                head,
            ],
        )?;
        performance.source_snapshot.elapsed_us += started.elapsed().as_micros();
        Ok(Self {
            _temporary: temporary,
            root,
        })
    }
}

impl GraphStatusInspector {
    fn inspect(repo_hint: &Path) -> Result<GraphStatusReport, String> {
        let started = std::time::Instant::now();
        let mut performance = GraphLifecycleObservability::default();
        let discovery_started = std::time::Instant::now();
        let repository = GitRepo::discover(repo_hint).map_err(|error| error.to_string())?;
        let root = repository.root();
        let head_sha = repository
            .head_commit()
            .map_err(|error| error.to_string())?;
        let tree_sha = git(root, &["rev-parse", &format!("{head_sha}^{{tree}}")])?;
        performance.repository_discovery.elapsed_us = discovery_started.elapsed().as_micros();
        let policy_started = std::time::Instant::now();
        let (policy, policy_bytes) = graph_policy_at_commit(&repository, &head_sha)?;
        let canonical_repository = policy
            .repository
            .clone()
            .unwrap_or_else(|| "unconfigured".into());
        let (pinned_engine_version, version_bytes) =
            engine_version_at_commit(&repository, &head_sha)?;
        performance.policy_loading.elapsed_us = policy_started.elapsed().as_micros();
        performance.policy_loading.bytes_read = policy_bytes + version_bytes;
        let running_version = env!("CARGO_PKG_VERSION").to_string();
        let compatibility = graph_compatibility(&policy, pinned_engine_version.as_deref());
        let source = GraphSourceState { head_sha, tree_sha };
        let policy_state = GraphPolicyState {
            authority: policy.authority,
            repository: policy.repository.clone(),
            policy_sha256: policy.digest(),
            pinned_engine_version: pinned_engine_version.clone(),
        };
        let installed = InstalledGraphRuntime {
            router_version: running_version.clone(),
            linked_engine_version: running_version.clone(),
            graph_indexer_version: running_version.clone(),
        };
        let work = GraphLifecycleWork::default();

        if compatibility == GraphRefreshCompatibility::AuthorityDisabled {
            performance.finish(started.elapsed().as_micros());
            return Ok(GraphStatusReport {
                schema_version: PLAN_SCHEMA_VERSION,
                canonical_repository,
                source,
                policy: policy_state,
                installed,
                fragments: FragmentState {
                    status: GraphIntegrityStatus::Disabled,
                    existing_file_count: 0,
                    proposed_file_count: 0,
                    changed_file_count: 0,
                    working_tree_matches_proposal: true,
                },
                derived_store: DerivedStoreState {
                    status: DerivedStoreStatus::Disabled,
                    indexed_commit: None,
                    action: DerivedStoreAction::None,
                },
                compatibility,
                healthy: true,
                action_required: false,
                safe_to_refresh: false,
                refresh_plan_required: false,
                blockers: Vec::new(),
                diagnosis: None,
                next_action: "graph authority is disabled; no action is required. Enroll explicitly with `aethyme deploy --repo . --with-graph`"
                    .into(),
                work,
                performance,
            });
        }

        let mut blockers = compatibility_blockers(compatibility, pinned_engine_version.as_deref());
        let validation_started = std::time::Instant::now();
        let committed_files = committed_graph_files(root)?;
        let committed_validation = validate_fragment_authority(
            root,
            &source.head_sha,
            &policy,
            pinned_engine_version.as_deref(),
            &committed_files,
        );
        let active_files = filesystem_graph_files(root)?;
        let active_validation = validate_fragment_authority(
            root,
            &source.head_sha,
            &policy,
            pinned_engine_version.as_deref(),
            &active_files,
        );
        let active_changes = compare_graph_files(&committed_files, &active_files);
        performance.fragment_validation.elapsed_us = validation_started.elapsed().as_micros();
        performance.fragment_validation.bytes_read =
            graph_files_bytes(&committed_files) + graph_files_bytes(&active_files);
        let working_tree_matches_proposal = active_validation.is_ok();
        let fragment_status = if compatibility != GraphRefreshCompatibility::Compatible {
            GraphIntegrityStatus::Incompatible
        } else if committed_validation.is_ok() {
            GraphIntegrityStatus::Passed
        } else {
            GraphIntegrityStatus::Stale
        };
        let diagnosis = committed_validation.err();
        blockers.sort();
        blockers.dedup();
        let derived_store = derived_store_state(root, &source.head_sha);
        let healthy = compatibility == GraphRefreshCompatibility::Compatible
            && fragment_status == GraphIntegrityStatus::Passed;
        let action_required = !healthy || derived_store.action != DerivedStoreAction::None;
        let next_action = if compatibility != GraphRefreshCompatibility::Compatible {
            "resolve the compatibility blocker before using graph artifacts".into()
        } else if working_tree_matches_proposal && !active_changes.is_empty() {
            "review and commit the generated graph paths, then rerun graph status; committed HEAD remains the authority"
                .into()
        } else if fragment_status != GraphIntegrityStatus::Passed {
            "review `aethyme graph refresh plan --repo . --diff` before regenerating authoritative fragments"
                .into()
        } else if derived_store.action != DerivedStoreAction::None {
            "run `aethyme graph materialize --repo .` to build the verified local query store"
                .into()
        } else {
            "graph fragments and derived store are current; no action is required".into()
        };
        performance.finish(started.elapsed().as_micros());
        Ok(GraphStatusReport {
            schema_version: PLAN_SCHEMA_VERSION,
            canonical_repository,
            source,
            policy: policy_state,
            installed,
            fragments: FragmentState {
                status: fragment_status,
                existing_file_count: committed_files.len(),
                proposed_file_count: if working_tree_matches_proposal {
                    active_files.len()
                } else {
                    committed_files.len()
                },
                changed_file_count: if working_tree_matches_proposal {
                    active_changes.len()
                } else {
                    0
                },
                working_tree_matches_proposal,
            },
            derived_store,
            compatibility,
            healthy,
            action_required,
            // Status is observational; only a digest-bound refresh plan checks
            // dirty overlap, sessions, leases, and exact writes.
            safe_to_refresh: false,
            refresh_plan_required: fragment_status != GraphIntegrityStatus::Passed
                || working_tree_matches_proposal && !active_changes.is_empty(),
            blockers,
            diagnosis,
            next_action,
            work,
            performance,
        })
    }
}

fn graph_policy_at_commit(
    repository: &GitRepo,
    commit: &str,
) -> Result<(GraphIntegrityPolicy, u64), String> {
    match repository
        .file_at_commit(commit, GRAPH_CONFIG_RELPATH)
        .map_err(|error| error.to_string())?
    {
        Some(text) => {
            let bytes = text.len() as u64;
            GraphIntegrityPolicy::parse(&text)
                .map(|policy| (policy, bytes))
                .map_err(|error| error.to_string())
        }
        None => Ok((GraphIntegrityPolicy::default(), 0)),
    }
}

fn engine_version_at_commit(
    repository: &GitRepo,
    commit: &str,
) -> Result<(Option<String>, u64), String> {
    let value = repository
        .file_at_commit(commit, ".aethyme/engine-version")
        .map_err(|error| error.to_string())?;
    let bytes = value.as_ref().map_or(0, |version| version.len() as u64);
    Ok((
        value
            .map(|version| version.trim().to_string())
            .filter(|version| !version.is_empty()),
        bytes,
    ))
}

fn graph_compatibility(
    policy: &GraphIntegrityPolicy,
    pinned_engine_version: Option<&str>,
) -> GraphRefreshCompatibility {
    if !policy.enforces_committed_fragments() {
        GraphRefreshCompatibility::AuthorityDisabled
    } else if pinned_engine_version.is_none() {
        GraphRefreshCompatibility::MissingEnginePin
    } else if pinned_engine_version != Some(env!("CARGO_PKG_VERSION")) {
        GraphRefreshCompatibility::VersionMismatch
    } else {
        GraphRefreshCompatibility::Compatible
    }
}

fn compatibility_blockers(
    compatibility: GraphRefreshCompatibility,
    pinned_engine_version: Option<&str>,
) -> Vec<String> {
    match compatibility {
        GraphRefreshCompatibility::AuthorityDisabled => Vec::new(),
        GraphRefreshCompatibility::MissingEnginePin => vec![
            "committed .aethyme/engine-version is missing or unreadable; never invent or rewrite the pin during refresh"
                .into(),
        ],
        GraphRefreshCompatibility::VersionMismatch => vec![format!(
            "repository graph pin {} does not match installed Aethyme {}; install the signed compatible release or migrate the repository policy explicitly",
            pinned_engine_version.unwrap_or("<missing>"),
            env!("CARGO_PKG_VERSION")
        )],
        GraphRefreshCompatibility::Compatible => Vec::new(),
    }
}

fn build_plan(repo_hint: &Path) -> Result<BuiltPlan, String> {
    let started = std::time::Instant::now();
    let mut work = GraphLifecycleWork::default();
    let mut performance = GraphLifecycleObservability::default();
    let discovery_started = std::time::Instant::now();
    let repository = GitRepo::discover(repo_hint).map_err(|error| error.to_string())?;
    let root = repository.root();
    let head_sha = repository
        .head_commit()
        .map_err(|error| error.to_string())?;
    let tree_sha = git(root, &["rev-parse", &format!("{head_sha}^{{tree}}")])?;
    performance.repository_discovery.elapsed_us = discovery_started.elapsed().as_micros();
    let proposal =
        ProposedRepository::from_committed_head(root, &head_sha, &mut work, &mut performance)?;
    let policy_started = std::time::Instant::now();
    let policy = GraphIntegrityPolicy::load(&proposal.root).map_err(|error| error.to_string())?;
    let canonical_repository = policy
        .repository
        .clone()
        .unwrap_or_else(|| "unconfigured".into());
    let pinned_engine_version = read_engine_version(&proposal.root).ok();
    performance.policy_loading.elapsed_us = policy_started.elapsed().as_micros();
    performance.policy_loading.bytes_read = file_len(&proposal.root.join(GRAPH_CONFIG_RELPATH))
        + file_len(&proposal.root.join(".aethyme/engine-version"));
    let running_version = env!("CARGO_PKG_VERSION").to_string();

    let compatibility = graph_compatibility(&policy, pinned_engine_version.as_deref());

    let validation_started = std::time::Instant::now();
    let existing = committed_graph_files(&proposal.root)?;
    performance.fragment_validation.elapsed_us += validation_started.elapsed().as_micros();
    performance.fragment_validation.bytes_read += graph_files_bytes(&existing);
    let mut blockers = Vec::new();
    let proposed = if compatibility == GraphRefreshCompatibility::Compatible {
        regenerate_fragments(
            &proposal.root,
            &canonical_repository,
            &running_version,
            &mut work,
            &mut performance,
        )?;
        let validation_started = std::time::Instant::now();
        let files = filesystem_graph_files(&proposal.root)?;
        performance.fragment_validation.elapsed_us += validation_started.elapsed().as_micros();
        performance.fragment_validation.bytes_read += graph_files_bytes(&files);
        files
    } else {
        match compatibility {
            GraphRefreshCompatibility::AuthorityDisabled => blockers.push(
                "repository graph authority is disabled; change policy through a reviewed repository upgrade before refreshing"
                    .into(),
            ),
            GraphRefreshCompatibility::MissingEnginePin => blockers.push(
                "committed .aethyme/engine-version is missing or unreadable; never invent or rewrite the pin during refresh"
                    .into(),
            ),
            GraphRefreshCompatibility::VersionMismatch => blockers.push(format!(
                "repository graph pin {} does not match installed Aethyme {}; install the signed compatible release or migrate the repository policy explicitly",
                pinned_engine_version.as_deref().unwrap_or("<missing>"),
                running_version
            )),
            GraphRefreshCompatibility::Compatible => {}
        }
        existing.clone()
    };
    let changes = compare_graph_files(&existing, &proposed);
    let fragment_status = if !policy.enforces_committed_fragments() {
        GraphIntegrityStatus::Disabled
    } else if compatibility != GraphRefreshCompatibility::Compatible {
        GraphIntegrityStatus::Incompatible
    } else if changes.is_empty() {
        GraphIntegrityStatus::Passed
    } else {
        GraphIntegrityStatus::Stale
    };
    let planned_paths = changes
        .iter()
        .map(|change| change.path.clone())
        .collect::<BTreeSet<_>>();
    let dirty_paths = repository
        .dirty_paths()
        .map_err(|error| error.to_string())?;
    let mut applied_uncommitted_paths = Vec::new();
    let mut overlapping_dirty_paths = Vec::new();
    let mut disjoint_dirty_paths = Vec::new();
    for path in &dirty_paths {
        if !planned_paths.contains(path) {
            disjoint_dirty_paths.push(path.clone());
        } else if path_crosses_symlink(root, path)? {
            overlapping_dirty_paths.push(path.clone());
        } else if active_path_matches(root, path, proposed.get(path))? {
            applied_uncommitted_paths.push(path.clone());
        } else {
            overlapping_dirty_paths.push(path.clone());
        }
    }
    if !overlapping_dirty_paths.is_empty() {
        blockers.push(format!(
            "uncommitted changes overlap graph outputs: {}",
            overlapping_dirty_paths.join(", ")
        ));
    }
    let (active_sessions, relevant_leases) = broker_preconditions(root, &planned_paths)?;
    if !active_sessions.is_empty() && !changes.is_empty() {
        blockers.push(format!(
            "authoritative graph refresh is blocked while broker sessions are live: {}",
            active_sessions
                .iter()
                .map(|session| session.session_id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for path in &planned_paths {
        if path_crosses_symlink(root, path)? {
            blockers.push(format!("graph output path {path} crosses a symlink"));
        }
    }
    blockers.sort();
    blockers.dedup();

    let derived_store = derived_store_state(root, &head_sha);
    let safe_to_execute = blockers.is_empty();
    let working_tree_matches_proposal = changes
        .iter()
        .all(|change| applied_uncommitted_paths.contains(&change.path));
    let next_action = if !safe_to_execute {
        "resolve every blocker, then regenerate the graph refresh plan".into()
    } else if changes.is_empty() && derived_store.action == DerivedStoreAction::None {
        "graph fragments and derived store are current; no execution is required".into()
    } else if working_tree_matches_proposal && derived_store.action == DerivedStoreAction::None {
        "review and commit the generated graph paths, then rerun graph status; committed HEAD remains the authority"
            .into()
    } else {
        "review the plan and run `aethyme graph refresh execute --repo . --confirm <plan-sha256>`"
            .into()
    };

    let policy_sha256 = policy.digest();
    performance.finish(started.elapsed().as_micros());
    let mut plan = GraphRefreshPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        plan_sha256: String::new(),
        canonical_repository,
        source: GraphSourceState { head_sha, tree_sha },
        policy: GraphPolicyState {
            authority: policy.authority,
            repository: policy.repository,
            policy_sha256,
            pinned_engine_version,
        },
        installed: InstalledGraphRuntime {
            router_version: running_version.clone(),
            linked_engine_version: running_version.clone(),
            graph_indexer_version: running_version,
        },
        fragments: FragmentState {
            status: fragment_status,
            existing_file_count: existing.len(),
            proposed_file_count: proposed.len(),
            changed_file_count: changes.len(),
            working_tree_matches_proposal,
        },
        derived_store,
        changes,
        dirty_paths,
        applied_uncommitted_paths,
        overlapping_dirty_paths,
        disjoint_dirty_paths,
        active_sessions,
        relevant_leases,
        compatibility,
        safe_to_execute,
        blockers,
        next_action,
        work,
        performance,
        cache: GraphMaterializationCache::default(),
    };
    plan.plan_sha256 = plan_digest(&plan)?;
    Ok(BuiltPlan {
        plan,
        existing_files: existing,
        proposed_files: proposed,
    })
}

impl GraphMaterializationPlan {
    fn build(repo_hint: &Path) -> Result<Self, String> {
        let status = GraphStatusInspector::inspect(repo_hint)?;
        if status.compatibility != GraphRefreshCompatibility::Compatible {
            return Err(format!(
                "graph materialization requires compatible committed graph authority; status is {:?}: {}",
                status.compatibility, status.next_action
            ));
        }
        if status.fragments.status != GraphIntegrityStatus::Passed {
            return Err(format!(
                "committed graph fragments do not match committed HEAD: {}; run `aethyme graph refresh plan --repo . --diff`",
                status
                    .diagnosis
                    .as_deref()
                    .unwrap_or("fragment authority is stale")
            ));
        }
        let repository = GitRepo::discover(repo_hint).map_err(|error| error.to_string())?;
        let validation_started = std::time::Instant::now();
        let committed_files = committed_graph_files(repository.root())?;
        let mut performance = status.performance;
        performance.fragment_validation.elapsed_us += validation_started.elapsed().as_micros();
        performance.fragment_validation.bytes_read += graph_files_bytes(&committed_files);
        Ok(Self {
            canonical_repository: status.canonical_repository,
            source_head: status.source.head_sha,
            fragment_status: status.fragments.status,
            action: status.derived_store.action,
            committed_files,
            work: status.work,
            performance,
        })
    }
}

struct ExactFragmentRepository {
    _temporary: tempfile::TempDir,
    root: PathBuf,
}

impl ExactFragmentRepository {
    fn from_committed_files(files: &BTreeMap<String, GraphFileBytes>) -> Result<Self, String> {
        let temporary = tempfile::Builder::new()
            .prefix("aethyme-graph-materialize-")
            .tempdir()
            .map_err(|error| format!("create exact fragment directory: {error}"))?;
        let root = temporary.path().join("repository");
        for (relative, file) in files {
            if !relative.starts_with(".aethyme/graph/")
                || Path::new(relative)
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err(format!("invalid committed graph path {relative}"));
            }
            let target = root.join(relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("create exact fragment parent: {error}"))?;
            }
            std::fs::write(&target, &file.bytes)
                .map_err(|error| format!("write exact committed fragment {relative}: {error}"))?;
            set_mode(&target, &file.mode)?;
        }
        Ok(Self {
            _temporary: temporary,
            root,
        })
    }
}

fn materialize(repo_hint: &Path) -> Result<GraphMaterializationReport, String> {
    let started = std::time::Instant::now();
    let mut plan = GraphMaterializationPlan::build(repo_hint)?;
    if plan.fragment_status != GraphIntegrityStatus::Passed {
        return Err("committed graph fragments do not match committed HEAD; run `aethyme graph refresh plan --repo . --diff`".into());
    }
    let repository = GitRepo::discover(repo_hint).map_err(|error| error.to_string())?;
    let _lock = (plan.action != DerivedStoreAction::None)
        .then(|| acquire_refresh_lock(&repository))
        .transpose()?;
    let current_head = repository
        .head_commit()
        .map_err(|error| error.to_string())?;
    if current_head != plan.source_head {
        return Err("repository HEAD moved during graph validation; retry materialization".into());
    }
    let mut file_count = 0;
    let action = plan.action;
    let mut cache = GraphMaterializationCache::default();
    if action != DerivedStoreAction::None {
        let current_head = repository
            .head_commit()
            .map_err(|error| error.to_string())?;
        if current_head != plan.source_head {
            return Err(
                "repository HEAD moved during graph validation; retry materialization".into(),
            );
        }
        let observation = materialize_verified_store(
            repository.root(),
            &plan.source_head,
            &plan.committed_files,
        )?;
        file_count = observation.counts.files.unwrap_or(0);
        plan.performance.counts = observation.counts;
        plan.performance.redb_materialization = observation.phase;
        cache = observation.cache;
    } else if let Ok(store) = GraphStore::open_read_only(repository.root())
        && let Ok(Some(metadata)) = store.repo_metadata()
    {
        file_count = metadata.file_count as usize;
        plan.performance.counts.files = Some(file_count);
    }
    plan.performance.finish(started.elapsed().as_micros());
    Ok(GraphMaterializationReport {
        schema_version: PLAN_SCHEMA_VERSION,
        canonical_repository: plan.canonical_repository,
        source_head: plan.source_head,
        fragment_status: plan.fragment_status,
        action,
        file_count,
        elapsed_ms: started.elapsed().as_millis(),
        work: plan.work,
        performance: plan.performance,
        cache,
    })
}

fn graph_counts(map: &aethyme_engine::map::RepositoryMap) -> GraphEntityCounts {
    GraphEntityCounts {
        files: Some(map.files.len()),
        nodes: Some(
            1 + map.areas.len()
                + map.directories.len()
                + map.files.len()
                + map.classes.len()
                + map.functions.len()
                + map.surfaces.len()
                + map.docs.len()
                + map.configs.len()
                + map.unresolved.len()
                + map.risk_flags.len(),
        ),
        edges: Some(map.edges.len()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphFileBytes {
    bytes: Vec<u8>,
    mode: String,
}

fn committed_graph_files(repo: &Path) -> Result<BTreeMap<String, GraphFileBytes>, String> {
    let output = git_bytes(
        repo,
        &["ls-tree", "-r", "-z", "HEAD", "--", ".aethyme/graph"],
    )?;
    let mut entries = Vec::new();
    for row in output
        .split(|byte| *byte == 0)
        .filter(|row| !row.is_empty())
    {
        let row = std::str::from_utf8(row)
            .map_err(|_| "committed graph contains a non-UTF-8 path".to_string())?;
        let (metadata, path) = row
            .split_once('\t')
            .ok_or_else(|| "invalid git ls-tree graph record".to_string())?;
        let mut metadata = metadata.split_whitespace();
        let mode = metadata
            .next()
            .ok_or_else(|| "missing graph file mode".to_string())?
            .to_string();
        let kind = metadata
            .next()
            .ok_or_else(|| "missing graph object kind".to_string())?;
        let oid = metadata
            .next()
            .ok_or_else(|| "missing graph object id".to_string())?
            .to_string();
        if kind != "blob" || oid.len() < 40 || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("invalid committed graph object for {path}"));
        }
        entries.push((path.to_string(), mode, oid));
    }

    let blobs = git_blob_batch(repo, entries.iter().map(|(_, _, oid)| oid.as_str()))?;
    let mut files = BTreeMap::new();
    for ((path, mode, _), bytes) in entries.into_iter().zip(blobs) {
        files.insert(path, GraphFileBytes { bytes, mode });
    }
    Ok(files)
}

fn git_blob_batch<'a>(
    repo: &Path,
    object_ids: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<Vec<u8>>, String> {
    let object_ids = object_ids
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if object_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = Command::new("git")
        .args(["cat-file", "--batch"])
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("git cat-file --batch: {error}"))?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| "git cat-file --batch stdin is unavailable".to_string())?;
    let requested_object_ids = object_ids.clone();
    let writer = std::thread::spawn(move || {
        for object_id in requested_object_ids {
            input
                .write_all(object_id.as_bytes())
                .and_then(|_| input.write_all(b"\n"))
                .map_err(|error| format!("write git cat-file batch request: {error}"))?;
        }
        Ok::<(), String>(())
    });
    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait for git cat-file --batch: {error}"))?;
    writer
        .join()
        .map_err(|_| "git cat-file batch request writer panicked".to_string())??;
    if !output.status.success() {
        return Err(format!(
            "git cat-file --batch: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let mut offset = 0usize;
    let mut blobs = Vec::with_capacity(object_ids.len());
    for expected_oid in object_ids {
        let header_end = output.stdout[offset..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|position| offset + position)
            .ok_or_else(|| "git cat-file batch response has no header terminator".to_string())?;
        let header = std::str::from_utf8(&output.stdout[offset..header_end])
            .map_err(|_| "git cat-file batch response header is not UTF-8".to_string())?;
        let mut fields = header.split_whitespace();
        let actual_oid = fields.next().unwrap_or_default();
        let kind = fields.next().unwrap_or_default();
        let size = fields
            .next()
            .ok_or_else(|| format!("git cat-file omitted size for {expected_oid}"))?
            .parse::<usize>()
            .map_err(|_| format!("git cat-file returned invalid size for {expected_oid}"))?;
        if actual_oid != expected_oid || kind != "blob" || fields.next().is_some() {
            return Err(format!(
                "git cat-file returned invalid object for {expected_oid}"
            ));
        }
        let content_start = header_end + 1;
        let content_end = content_start
            .checked_add(size)
            .filter(|end| *end < output.stdout.len())
            .ok_or_else(|| format!("git cat-file truncated object {expected_oid}"))?;
        if output.stdout[content_end] != b'\n' {
            return Err(format!("git cat-file omitted delimiter for {expected_oid}"));
        }
        blobs.push(output.stdout[content_start..content_end].to_vec());
        offset = content_end + 1;
    }
    if offset != output.stdout.len() {
        return Err("git cat-file returned unexpected trailing bytes".into());
    }
    Ok(blobs)
}

fn validate_fragment_authority(
    repo: &Path,
    head: &str,
    policy: &GraphIntegrityPolicy,
    pinned_engine_version: Option<&str>,
    files: &BTreeMap<String, GraphFileBytes>,
) -> Result<(), String> {
    let manifest_bytes = files
        .get(GRAPH_MANIFEST_RELPATH)
        .ok_or_else(|| "committed graph authority manifest is missing".to_string())?;
    let manifest =
        GraphAuthorityManifest::decode(&manifest_bytes.bytes).map_err(|error| error.to_string())?;
    let repository = policy
        .repository
        .as_deref()
        .ok_or_else(|| "graph.repository is missing".to_string())?;
    if manifest.repository != repository {
        return Err(format!(
            "graph manifest repository {:?} does not match policy {:?}",
            manifest.repository, repository
        ));
    }
    let pinned_engine_version = pinned_engine_version
        .ok_or_else(|| "committed .aethyme/engine-version is missing".to_string())?;
    if manifest.engine_version != pinned_engine_version {
        return Err(format!(
            "graph manifest engine {} does not match pin {}",
            manifest.engine_version, pinned_engine_version
        ));
    }
    let source_tree_sha256 =
        committed_source_tree_digest(repo, head).map_err(|error| error.to_string())?;
    if manifest.source_tree_sha256 != source_tree_sha256 {
        return Err("committed graph manifest does not match exact source tree".into());
    }
    let fragment_set_sha256 = graph_fragment_set_digest(
        files
            .iter()
            .map(|(path, file)| (path.as_str(), file.mode.as_str(), file.bytes.as_slice())),
    );
    if manifest.fragment_set_sha256 != fragment_set_sha256 {
        return Err("committed graph manifest does not match fragment bytes".into());
    }
    Ok(())
}

fn regenerate_fragments(
    repo: &Path,
    repository: &str,
    version: &str,
    work: &mut GraphLifecycleWork,
    performance: &mut GraphLifecycleObservability,
) -> Result<(), String> {
    let graph = repo.join(".aethyme/graph");
    if let Err(error) = std::fs::remove_dir_all(&graph)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(format!("reset disposable graph output: {error}"));
    }
    let context = IndexerContext::new(repository, repo.to_path_buf(), version)
        .map_err(|error| error.to_string())?;
    work.source_index_runs += 1;
    let summary =
        index_repo_to_disk(&context, &WalkOptions::default()).map_err(|error| error.to_string())?;
    performance.source_indexing.elapsed_us += summary.observability.source_discovery_elapsed_us
        + summary.observability.source_indexing_elapsed_us;
    performance.source_indexing.bytes_read += summary.observability.source_bytes_read;
    performance.fragment_serialization.elapsed_us +=
        summary.observability.fragment_serialization_elapsed_us;
    let linking_started = std::time::Instant::now();
    link_repo(&context).map_err(|error| error.to_string())?;
    performance.graph_linking.elapsed_us += linking_started.elapsed().as_micros();
    let serialization_started = std::time::Instant::now();
    write_graph_authority_manifest(repo, "HEAD", repository, version)
        .map_err(|error| error.to_string())?;
    performance.fragment_serialization.elapsed_us += serialization_started.elapsed().as_micros();
    performance.counts = GraphEntityCounts {
        files: Some(summary.total_files),
        nodes: Some(summary.total_nodes),
        edges: Some(summary.total_edges),
    };
    performance.fragment_serialization.bytes_written = directory_regular_file_bytes(&graph)?;
    Ok(())
}

fn filesystem_graph_files(repo: &Path) -> Result<BTreeMap<String, GraphFileBytes>, String> {
    let root = repo.join(".aethyme/graph");
    let mut paths = Vec::new();
    collect_regular_files(&root, &mut paths)?;
    paths.sort();
    let mut files = BTreeMap::new();
    for path in paths {
        let relative = path
            .strip_prefix(repo)
            .map_err(|_| "graph output escaped disposable repository".to_string())?
            .to_str()
            .ok_or_else(|| "generated graph contains a non-UTF-8 path".to_string())?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("read generated graph path {relative}: {error}"))?;
        files.insert(
            relative,
            GraphFileBytes {
                bytes,
                mode: "100644".into(),
            },
        );
    }
    Ok(files)
}

fn graph_files_bytes(files: &BTreeMap<String, GraphFileBytes>) -> u64 {
    files.values().map(|file| file.bytes.len() as u64).sum()
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn directory_regular_file_bytes(root: &Path) -> Result<u64, String> {
    let mut paths = Vec::new();
    collect_regular_files(root, &mut paths)?;
    Ok(paths.iter().map(|path| file_len(path)).sum())
}

fn collect_regular_files(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("read generated graph directory: {error}")),
    };
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let metadata = entry
            .file_type()
            .map_err(|error| format!("inspect generated graph output: {error}"))?;
        if metadata.is_symlink() {
            return Err("generated graph output contains a symlink".into());
        }
        if metadata.is_dir() {
            collect_regular_files(&entry.path(), output)?;
        } else if metadata.is_file() {
            output.push(entry.path());
        }
    }
    Ok(())
}

fn compare_graph_files(
    before: &BTreeMap<String, GraphFileBytes>,
    after: &BTreeMap<String, GraphFileBytes>,
) -> Vec<GraphFileChange> {
    let paths = before
        .keys()
        .chain(after.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    paths
        .into_iter()
        .filter_map(|path| {
            let old = before.get(&path);
            let new = after.get(&path);
            if old.is_some_and(|old| {
                new.is_some_and(|new| old.bytes == new.bytes && old.mode == new.mode)
            }) {
                return None;
            }
            Some(GraphFileChange {
                path,
                action: match (old, new) {
                    (None, Some(_)) => GraphFileAction::Create,
                    (Some(_), None) => GraphFileAction::Delete,
                    (Some(_), Some(_)) => GraphFileAction::Update,
                    (None, None) => unreachable!(),
                },
                before_sha256: old.map(|file| sha256(&file.bytes)),
                after_sha256: new.map(|file| sha256(&file.bytes)),
                before_mode: old.map(|file| file.mode.clone()),
                after_mode: new.map(|file| file.mode.clone()),
            })
        })
        .collect()
}

fn broker_preconditions(
    repo: &Path,
    planned_paths: &BTreeSet<String>,
) -> Result<(Vec<GraphSessionPrecondition>, Vec<GraphLeasePrecondition>), String> {
    let repository = GitRepo::discover(repo).map_err(|error| error.to_string())?;
    let main_root = repository.main_root().map_err(|error| error.to_string())?;
    if !main_root.join(aethyme_broker::BROKER_DB_RELPATH).is_file() {
        return Ok((Vec::new(), Vec::new()));
    }
    let broker = Broker::open_snapshot(repo).map_err(|error| error.to_string())?;
    let status = broker
        .status_snapshot(now_ms())
        .map_err(|error| error.to_string())?;
    let mut sessions = status
        .agents
        .into_iter()
        .filter(|agent| !agent.derived_status.is_closed())
        .map(|agent| GraphSessionPrecondition {
            session_id: agent.session.id,
            status: agent.derived_status,
        })
        .collect::<Vec<_>>();
    sessions.sort_by_key(|session| session.session_id);
    let mut leases = status
        .leases
        .into_iter()
        .filter(|lease| {
            lease.released_at.is_none()
                && planned_paths
                    .iter()
                    .any(|path| paths_overlap(&lease.path, path))
        })
        .map(|lease| GraphLeasePrecondition {
            session_id: lease.session_id,
            path: lease.path,
            kind: lease.kind,
        })
        .collect::<Vec<_>>();
    leases.sort_by(|left, right| {
        left.session_id
            .cmp(&right.session_id)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok((sessions, leases))
}

fn paths_overlap(left: &str, right: &str) -> bool {
    let left = left.trim_end_matches('/');
    let right = right.trim_end_matches('/');
    left == right
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn derived_store_state(repo: &Path, head: &str) -> DerivedStoreState {
    let path = GraphStore::final_path(repo);
    if !path.is_file() {
        return DerivedStoreState {
            status: DerivedStoreStatus::Missing,
            indexed_commit: None,
            action: DerivedStoreAction::MaterializeAfterFragmentVerification,
        };
    }
    match GraphStore::open_read_only(repo).and_then(|store| store.repo_metadata()) {
        Ok(Some(metadata)) if metadata.commit_hash.as_deref() == Some(head) => DerivedStoreState {
            status: DerivedStoreStatus::Current,
            indexed_commit: metadata.commit_hash,
            action: DerivedStoreAction::None,
        },
        Ok(metadata) => DerivedStoreState {
            status: DerivedStoreStatus::Stale,
            indexed_commit: metadata.and_then(|metadata| metadata.commit_hash),
            action: DerivedStoreAction::ReplaceAfterFragmentVerification,
        },
        Err(_) => DerivedStoreState {
            status: DerivedStoreStatus::Unreadable,
            indexed_commit: None,
            action: DerivedStoreAction::ReplaceAfterFragmentVerification,
        },
    }
}

fn path_crosses_symlink(repo: &Path, relative: &str) -> Result<bool, String> {
    let mut current = repo.to_path_buf();
    for component in Path::new(relative).components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(format!("inspect graph output path {relative}: {error}")),
        }
    }
    Ok(false)
}

fn plan_digest(plan: &GraphRefreshPlan) -> Result<String, String> {
    let mut normalized = plan.clone();
    normalized.plan_sha256.clear();
    // Timing and process memory are evidence from this invocation, never part
    // of the authorization digest for the deterministic proposed tree.
    normalized.performance = GraphLifecycleObservability::default();
    normalized.cache = GraphMaterializationCache::default();
    let bytes = serde_json::to_vec(&normalized).map_err(|error| error.to_string())?;
    Ok(sha256(&bytes))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphRefreshJournal {
    schema_version: u32,
    plan_sha256: String,
    source_head: String,
    canonical_repository: String,
    engine_version: String,
    entries: Vec<GraphRefreshJournalEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GraphRefreshJournalEntry {
    path: String,
    before: Option<GraphFileBytes>,
    after: Option<GraphFileBytes>,
}

struct GraphRefreshLock {
    file: File,
}

#[cfg(unix)]
impl Drop for GraphRefreshLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn execute(repo_hint: &Path, confirmation: &str) -> Result<GraphRefreshPlan, String> {
    let started = std::time::Instant::now();
    validate_digest(confirmation, "--confirm")?;
    let repository = GitRepo::discover(repo_hint).map_err(|error| error.to_string())?;
    let root = repository.root().to_path_buf();
    let _lock = acquire_refresh_lock(&repository)?;
    let mut built = build_plan(&root)?;
    if built.plan.plan_sha256 != confirmation {
        return Err(format!(
            "graph refresh state changed after review; planned {}, received {confirmation}; regenerate the plan",
            built.plan.plan_sha256
        ));
    }
    if !built.plan.safe_to_execute {
        return Err(format!(
            "graph refresh is blocked: {}",
            built.plan.blockers.join("; ")
        ));
    }
    if (built.plan.changes.is_empty() || built.plan.fragments.working_tree_matches_proposal)
        && built.plan.derived_store.action == DerivedStoreAction::None
    {
        return Ok(built.plan);
    }

    let journal = build_journal(&root, &built)?;
    let path = journal_path(&repository, confirmation)?;
    write_journal(&path, &journal)?;
    let result = (|| {
        let application_started = std::time::Instant::now();
        apply_journal_forward(&root, &journal)?;
        built.plan.performance.fragment_application.elapsed_us =
            application_started.elapsed().as_micros();
        built.plan.performance.fragment_application.bytes_written = journal
            .entries
            .iter()
            .filter_map(|entry| entry.after.as_ref())
            .map(|file| file.bytes.len() as u64)
            .sum();
        let observation = materialize_exact_store(&root, &journal.source_head)?;
        verify_journal_after(&root, &journal)?;
        Ok::<_, String>(observation)
    })();
    let observation = match result {
        Ok(observation) => observation,
        Err(error) => match rollback_journal(&root, &journal) {
            Ok(()) => {
                let _ = std::fs::remove_file(&path);
                return Err(format!(
                    "graph refresh failed and fragment files were rolled back: {error}"
                ));
            }
            Err(rollback_error) => {
                return Err(format!(
                    "graph refresh failed: {error}; rollback also failed: {rollback_error}; run `aethyme graph refresh recover --repo . --plan {confirmation}`"
                ));
            }
        },
    };
    std::fs::remove_file(&path)
        .map_err(|error| format!("remove completed graph refresh journal: {error}"))?;
    sync_parent(&path)?;
    built.plan.performance.redb_materialization = observation.phase;
    built.plan.performance.counts = observation.counts;
    built.plan.cache = observation.cache;
    built.plan.performance.finish(started.elapsed().as_micros());
    Ok(built.plan)
}

fn recover(repo_hint: &Path, digest: &str) -> Result<(), String> {
    validate_digest(digest, "--plan")?;
    let repository = GitRepo::discover(repo_hint).map_err(|error| error.to_string())?;
    let root = repository.root().to_path_buf();
    let _lock = acquire_refresh_lock(&repository)?;
    let path = journal_path(&repository, digest)?;
    let bytes = std::fs::read(&path).map_err(|error| {
        format!("no recoverable graph refresh journal for plan {digest}: {error}")
    })?;
    let journal: GraphRefreshJournal = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid graph refresh recovery journal: {error}"))?;
    if journal.schema_version != PLAN_SCHEMA_VERSION || journal.plan_sha256 != digest {
        return Err("graph refresh recovery journal does not match the requested plan".into());
    }
    let current_head = repository
        .head_commit()
        .map_err(|error| error.to_string())?;
    if current_head != journal.source_head {
        return Err(format!(
            "repository HEAD moved after the interrupted refresh; planned {}, found {current_head}; preserve the journal and restore the reviewed HEAD before recovery",
            journal.source_head
        ));
    }
    apply_journal_forward(&root, &journal)?;
    let _ = materialize_exact_store(&root, &journal.source_head)?;
    verify_journal_after(&root, &journal)?;
    std::fs::remove_file(&path)
        .map_err(|error| format!("remove completed graph refresh journal: {error}"))?;
    sync_parent(&path)
}

fn build_journal(repo: &Path, built: &BuiltPlan) -> Result<GraphRefreshJournal, String> {
    let mut entries = Vec::with_capacity(built.plan.changes.len());
    for change in &built.plan.changes {
        let current = read_active_file(repo, &change.path)?;
        let before = built.existing_files.get(&change.path).cloned();
        if before.as_ref().map(|file| sha256(&file.bytes)) != change.before_sha256 {
            return Err(format!(
                "graph output {} changed after planning; regenerate the plan",
                change.path
            ));
        }
        let after = built.proposed_files.get(&change.path).cloned();
        if after.as_ref().map(|file| sha256(&file.bytes)) != change.after_sha256 {
            return Err(format!(
                "disposable graph output {} does not match its reviewed hash",
                change.path
            ));
        }
        if !same_file(current.as_ref(), before.as_ref())
            && !same_file(current.as_ref(), after.as_ref())
        {
            return Err(format!(
                "graph output {} contains unreviewed bytes; regenerate the plan",
                change.path
            ));
        }
        entries.push(GraphRefreshJournalEntry {
            path: change.path.clone(),
            before,
            after,
        });
    }
    Ok(GraphRefreshJournal {
        schema_version: PLAN_SCHEMA_VERSION,
        plan_sha256: built.plan.plan_sha256.clone(),
        source_head: built.plan.source.head_sha.clone(),
        canonical_repository: built.plan.canonical_repository.clone(),
        engine_version: built.installed_version().to_string(),
        entries,
    })
}

impl BuiltPlan {
    fn installed_version(&self) -> &str {
        &self.plan.installed.linked_engine_version
    }
}

fn apply_journal_forward(repo: &Path, journal: &GraphRefreshJournal) -> Result<(), String> {
    let mut applied = 0usize;
    for entry in &journal.entries {
        if path_crosses_symlink(repo, &entry.path)? {
            return Err(format!(
                "graph output path {} crosses a symlink",
                entry.path
            ));
        }
        let current = read_active_file(repo, &entry.path)?;
        if same_file(current.as_ref(), entry.after.as_ref()) {
            continue;
        }
        if !same_file(current.as_ref(), entry.before.as_ref()) {
            return Err(format!(
                "graph output {} contains unreviewed bytes; recovery refuses to overwrite it",
                entry.path
            ));
        }
        let target = repo.join(&entry.path);
        match &entry.after {
            Some(after) => atomic_replace(&target, after, &journal.plan_sha256)?,
            None => {
                if target.exists() {
                    std::fs::remove_file(&target)
                        .map_err(|error| format!("delete graph output {}: {error}", entry.path))?;
                    sync_parent(&target)?;
                }
            }
        }
        applied += 1;
        crash_after_applied_file_for_test(applied);
    }
    verify_journal_after(repo, journal)
}

#[cfg(debug_assertions)]
fn crash_after_applied_file_for_test(applied: usize) {
    let requested = std::env::var("AETHYME_TEST_GRAPH_REFRESH_CRASH_AFTER_FILES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    if requested == Some(applied) {
        panic!("test-injected graph refresh interruption after {applied} file(s)");
    }
}

#[cfg(not(debug_assertions))]
fn crash_after_applied_file_for_test(_applied: usize) {}

fn rollback_journal(repo: &Path, journal: &GraphRefreshJournal) -> Result<(), String> {
    for entry in journal.entries.iter().rev() {
        let target = repo.join(&entry.path);
        match &entry.before {
            Some(before) => atomic_replace(&target, before, &journal.plan_sha256)?,
            None => {
                if target.exists() {
                    std::fs::remove_file(&target).map_err(|error| {
                        format!("remove newly-created graph output {}: {error}", entry.path)
                    })?;
                    sync_parent(&target)?;
                }
            }
        }
    }
    Ok(())
}

fn verify_journal_after(repo: &Path, journal: &GraphRefreshJournal) -> Result<(), String> {
    for entry in &journal.entries {
        let current = read_active_file(repo, &entry.path)?;
        if !same_file(current.as_ref(), entry.after.as_ref()) {
            return Err(format!(
                "post-refresh verification failed for {}",
                entry.path
            ));
        }
    }
    Ok(())
}

struct GraphStoreMaterialization {
    phase: aethyme_graph_storage::GraphPhaseObservation,
    counts: GraphEntityCounts,
    cache: GraphMaterializationCache,
}

fn materialize_exact_store(repo: &Path, head: &str) -> Result<GraphStoreMaterialization, String> {
    let files = filesystem_graph_files(repo)?;
    materialize_verified_store(repo, head, &files)
}

fn materialize_verified_store(
    repo: &Path,
    head: &str,
    files: &BTreeMap<String, GraphFileBytes>,
) -> Result<GraphStoreMaterialization, String> {
    let started = std::time::Instant::now();
    let bytes_read = graph_files_bytes(&files);
    let manifest_bytes = files
        .get(GRAPH_MANIFEST_RELPATH)
        .ok_or_else(|| "graph authority manifest is missing during materialization".to_string())?;
    let manifest = GraphAuthorityManifest::decode(&manifest_bytes.bytes)
        .map_err(|error| format!("decode graph authority manifest for caching: {error}"))?;
    let key = GraphStoreCacheKey {
        source_tree_sha256: manifest.source_tree_sha256,
        fragment_manifest_sha256: sha256(&manifest_bytes.bytes),
        engine_version: manifest.engine_version,
        engine_protocol_version: aethyme_engine::daemon::ENGINE_PROTOCOL_VERSION,
        storage_schema_version:
            aethyme_engine::store::redb::graph_store::GRAPH_STORE_SCHEMA_VERSION,
    };
    let key_sha256 = key.digest()?;
    let cache = GraphStoreArtifactCache::for_environment(repo);
    let entry = cache.as_ref().and_then(|cache| cache.acquire(key).ok());
    if let Some(entry) = &entry
        && let Ok(Some(artifact)) = entry.lookup()
        && let Ok(metadata) = GraphStore::install_cached_artifact(repo, &artifact.path, head)
    {
        return Ok(GraphStoreMaterialization {
            phase: aethyme_graph_storage::GraphPhaseObservation {
                elapsed_us: started.elapsed().as_micros(),
                bytes_read: artifact.bytes,
                bytes_written: file_len(&GraphStore::final_path(repo)),
            },
            counts: GraphEntityCounts {
                files: Some(metadata.file_count as usize),
                nodes: None,
                edges: None,
            },
            cache: GraphMaterializationCache {
                status: GraphMaterializationCacheStatus::Hit,
                key_sha256: Some(key_sha256),
                artifact_bytes: artifact.bytes,
            },
        });
    }

    let exact = ExactFragmentRepository::from_committed_files(&files)?;
    let (mut map, _) = aethyme_engine::map::RepositoryMap::build_from_fragments(&exact.root)?;
    let counts = graph_counts(&map);
    map.snapshot.root = repo
        .canonicalize()
        .map_err(|error| format!("resolve graph store target: {error}"))?
        .to_string_lossy()
        .into_owned();
    aethyme_engine::index_store::materialize_graph_store(repo, &map, head)?;
    let store_path = GraphStore::final_path(repo);
    let artifact_bytes = file_len(&store_path);
    let cache_status = match &entry {
        Some(entry) if entry.store(&store_path).is_ok() => {
            GraphMaterializationCacheStatus::MissStored
        }
        Some(_) => GraphMaterializationCacheStatus::MissUnstored,
        None if cache.is_some() => GraphMaterializationCacheStatus::MissUnstored,
        None => GraphMaterializationCacheStatus::Unavailable,
    };
    Ok(GraphStoreMaterialization {
        phase: aethyme_graph_storage::GraphPhaseObservation {
            elapsed_us: started.elapsed().as_micros(),
            bytes_read,
            bytes_written: artifact_bytes,
        },
        counts,
        cache: GraphMaterializationCache {
            status: cache_status,
            key_sha256: Some(key_sha256),
            artifact_bytes,
        },
    })
}

fn acquire_refresh_lock(repository: &GitRepo) -> Result<GraphRefreshLock, String> {
    let directory = graph_refresh_state_dir(repository)?;
    let path = directory.join("refresh.lock");
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("open graph refresh lock: {error}"))?;
    #[cfg(unix)]
    {
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err("another graph refresh or recovery holds the repository lock".into());
        }
    }
    #[cfg(not(unix))]
    {
        return Err("transactional graph refresh requires Unix file locking".into());
    }
    file.set_len(0)
        .map_err(|error| format!("initialize graph refresh lock: {error}"))?;
    writeln!(file, "pid={}", std::process::id())
        .map_err(|error| format!("initialize graph refresh lock: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("sync graph refresh lock: {error}"))?;
    Ok(GraphRefreshLock { file })
}

fn graph_refresh_state_dir(repository: &GitRepo) -> Result<PathBuf, String> {
    let canonical = repository
        .root()
        .canonicalize()
        .map_err(|error| format!("resolve graph refresh repository: {error}"))?;
    let key = sha256(canonical.to_string_lossy().as_bytes());
    let directory = repository
        .git_common_dir()
        .map_err(|error| error.to_string())?
        .join("aethyme-graph-refresh")
        .join(key);
    if let Ok(metadata) = std::fs::symlink_metadata(&directory)
        && (metadata.file_type().is_symlink() || !metadata.is_dir())
    {
        return Err("graph refresh state path must be a private directory".into());
    }
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("create graph refresh state directory: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("protect graph refresh state directory: {error}"))?;
    }
    Ok(directory)
}

fn journal_path(repository: &GitRepo, digest: &str) -> Result<PathBuf, String> {
    Ok(graph_refresh_state_dir(repository)?.join(format!("{digest}.rollback.json")))
}

fn write_journal(path: &Path, journal: &GraphRefreshJournal) -> Result<(), String> {
    let bytes = serde_json::to_vec(journal).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("rollback.json.tmp");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("create graph refresh journal: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("protect graph refresh journal: {error}"))?;
    }
    file.write_all(&bytes)
        .map_err(|error| format!("write graph refresh journal: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("sync graph refresh journal: {error}"))?;
    std::fs::rename(&temporary, path)
        .map_err(|error| format!("publish graph refresh journal: {error}"))?;
    sync_parent(path)
}

fn atomic_replace(target: &Path, file: &GraphFileBytes, digest: &str) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| "graph output has no parent directory".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create graph output directory: {error}"))?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "graph output filename is not UTF-8".to_string())?;
    let temporary = parent.join(format!(".{name}.aethyme-{}.tmp", short(digest)));
    if temporary.exists() {
        std::fs::remove_file(&temporary)
            .map_err(|error| format!("remove stale graph temporary file: {error}"))?;
    }
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("create graph temporary file: {error}"))?;
    output
        .write_all(&file.bytes)
        .map_err(|error| format!("write graph temporary file: {error}"))?;
    output
        .sync_all()
        .map_err(|error| format!("sync graph temporary file: {error}"))?;
    set_mode(&temporary, &file.mode)?;
    std::fs::rename(&temporary, target)
        .map_err(|error| format!("replace graph output {}: {error}", target.display()))?;
    sync_parent(target)
}

fn read_active_file(repo: &Path, relative: &str) -> Result<Option<GraphFileBytes>, String> {
    let path = repo.join(relative);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("inspect graph output {relative}: {error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("graph output {relative} must be a regular file"));
    }
    let bytes =
        std::fs::read(&path).map_err(|error| format!("read graph output {relative}: {error}"))?;
    Ok(Some(GraphFileBytes {
        bytes,
        mode: file_mode(&metadata),
    }))
}

fn active_path_matches(
    repo: &Path,
    relative: &str,
    proposed: Option<&GraphFileBytes>,
) -> Result<bool, String> {
    let active = read_active_file(repo, relative)?;
    Ok(same_file(active.as_ref(), proposed))
}

fn same_file(left: Option<&GraphFileBytes>, right: Option<&GraphFileBytes>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.bytes == right.bytes && left.mode == right.mode,
        _ => false,
    }
}

#[cfg(unix)]
fn file_mode(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o111 == 0 {
        "100644".into()
    } else {
        "100755".into()
    }
}

#[cfg(not(unix))]
fn file_mode(_metadata: &std::fs::Metadata) -> String {
    "100644".into()
}

fn set_mode(path: &Path, mode: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = if mode == "100755" { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(permissions))
            .map_err(|error| format!("set graph output mode: {error}"))?;
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "managed graph path has no parent".to_string())?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync graph directory {}: {error}", parent.display()))
}

fn validate_digest(value: &str, flag: &str) -> Result<(), String> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(format!("{flag} must be the full 64-character plan SHA-256"))
    }
}

fn render_status(status: &GraphStatusReport, json: bool) -> Result<(), String> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(status).map_err(|error| error.to_string())?
        );
    } else {
        println!("Graph status for {}", status.canonical_repository);
        println!("  source HEAD: {}", short(&status.source.head_sha));
        println!("  fragments: {:?}", status.fragments.status);
        println!("  derived store: {:?}", status.derived_store.status);
        println!("  compatibility: {:?}", status.compatibility);
        println!("  healthy: {}", status.healthy);
        println!("  action required: {}", status.action_required);
        render_performance_text(&status.performance);
        println!("  next: {}", status.next_action);
    }
    Ok(())
}

fn render_plan_text(plan: &GraphRefreshPlan) {
    println!("Graph refresh plan for {}", plan.canonical_repository);
    println!("  source HEAD: {}", plan.source.head_sha);
    println!("  tree: {}", plan.source.tree_sha);
    println!("  policy: {}", plan.policy.policy_sha256);
    println!("  fragment writes: {}", plan.changes.len());
    println!("  derived store: {:?}", plan.derived_store.action);
    println!("  safe: {}", plan.safe_to_execute);
    render_performance_text(&plan.performance);
    for blocker in &plan.blockers {
        println!("  blocker: {blocker}");
    }
    println!("  plan SHA-256: {}", plan.plan_sha256);
    println!("  next: {}", plan.next_action);
}

fn render_performance_text(performance: &GraphLifecycleObservability) {
    println!(
        "  performance: {:.3} ms total; {} bytes read; {} bytes written; peak RSS {}",
        performance.totals.elapsed_us as f64 / 1_000.0,
        performance.totals.bytes_read,
        performance.totals.bytes_written,
        performance
            .peak_memory_bytes
            .map(|bytes| format!("{bytes} bytes"))
            .unwrap_or_else(|| "unavailable".into())
    );
}

fn render_cache_text(cache: &GraphMaterializationCache) {
    if cache.status != GraphMaterializationCacheStatus::NotNeeded {
        println!(
            "  host cache: {:?}{} ({} bytes)",
            cache.status,
            cache
                .key_sha256
                .as_deref()
                .map(|key| format!(" {}", short(key)))
                .unwrap_or_default(),
            cache.artifact_bytes
        );
    }
}

fn render_change_summary(plan: &GraphRefreshPlan) -> String {
    let mut output = String::from("Graph fragment changes (hash-only; no source content):\n");
    if plan.changes.is_empty() {
        output.push_str("  none\n");
        return output;
    }
    for change in &plan.changes {
        output.push_str(&format!(
            "  {:?} {} {} -> {}\n",
            change.action,
            change.path,
            change.before_sha256.as_deref().map(short).unwrap_or("-"),
            change.after_sha256.as_deref().map(short).unwrap_or("-"),
        ));
    }
    output
}

fn option_path(args: &[String], name: &str) -> Result<PathBuf, String> {
    let value = args
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
        .unwrap_or(".");
    Ok(PathBuf::from(value))
}

fn required_option<'a>(args: &'a [String], name: &str) -> Result<&'a str, String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
        .ok_or_else(|| format!("missing required {name} value"))
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|_| format!("git {} returned non-UTF-8 text", args.join(" ")))
}

fn git_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn short(value: &str) -> &str {
    &value[..value.len().min(12)]
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batched_blob_reads_do_not_deadlock_when_both_pipes_exceed_capacity() {
        let temporary = tempfile::tempdir().unwrap();
        git(temporary.path(), &["init", "-q"]).unwrap();

        let mut child = Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(temporary.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&vec![b'x'; 2_048])
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let oid = String::from_utf8(output.stdout).unwrap().trim().to_string();

        // The request stream and response stream both exceed ordinary pipe
        // capacities. A write-all-then-read implementation blocks forever.
        let blobs = git_blob_batch(temporary.path(), vec![oid.as_str(); 4_096]).unwrap();
        assert_eq!(blobs.len(), 4_096);
        assert!(blobs.iter().all(|blob| blob.len() == 2_048));
    }
}
