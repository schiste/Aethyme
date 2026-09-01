//! Reviewed first-enrollment publication.
//!
//! Planning is observational: remote Git inspection and proposal generation
//! happen in a disposable clone, never in the operator's checkout.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::repository_upgrade::{RepositoryTreeChange, RepositoryUpgradePlan};

const PLAN_SCHEMA_VERSION: u32 = 1;
const RUNTIME_EXCLUDE_BLOCK: &str = "# BEGIN Aethyme first-enrollment runtime\n/.aethyme/\n# END Aethyme first-enrollment runtime\n";

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HookManagerKind {
    Absent,
    AethymeManaged,
    Foreign,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HookManagerState {
    pub kind: HookManagerKind,
    pub configured_value_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnrollmentRemoteBase {
    pub remote: String,
    pub default_branch_ref: String,
    pub exact_sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnrollmentLocalRef {
    pub ref_name: String,
    pub exact_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnrollmentPreservationRef {
    pub ref_name: String,
    pub target_sha: String,
    pub existing_sha: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FirstEnrollmentPlan {
    pub schema_version: u32,
    pub remote_base: EnrollmentRemoteBase,
    pub local_default: EnrollmentLocalRef,
    pub integration: EnrollmentLocalRef,
    pub local_ahead_upstream_commits: u64,
    pub local_behind_upstream_commits: u64,
    pub integration_can_rebase_to_upstream: bool,
    pub generated_tree: RepositoryUpgradePlan,
    pub generated_changes: Vec<RepositoryTreeChange>,
    pub planned_paths: Vec<String>,
    pub dirty_paths: Vec<String>,
    pub overlapping_dirty_paths: Vec<String>,
    pub disjoint_dirty_paths: Vec<String>,
    pub live_session_ids: Vec<i64>,
    pub nonterminal_queue_entry_ids: Vec<i64>,
    pub hook_manager: HookManagerState,
    pub shared_activation_present: bool,
    pub shared_activation_sha256: Option<String>,
    pub local_exclude_before_sha256: String,
    pub local_exclude_after_sha256: String,
    pub local_exclude_update_required: bool,
    pub preservation_refs: Vec<EnrollmentPreservationRef>,
    pub safe: bool,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    pub plan_digest: String,
    pub next_action: String,
}

#[derive(Serialize)]
struct FirstEnrollmentDigest<'a> {
    schema_version: u32,
    remote_base: &'a EnrollmentRemoteBase,
    local_default: &'a EnrollmentLocalRef,
    integration: &'a EnrollmentLocalRef,
    local_ahead_upstream_commits: u64,
    local_behind_upstream_commits: u64,
    integration_can_rebase_to_upstream: bool,
    generated_plan_digest: &'a str,
    generated_changes: &'a [RepositoryTreeChange],
    planned_paths: &'a [String],
    dirty_paths: &'a [String],
    overlapping_dirty_paths: &'a [String],
    disjoint_dirty_paths: &'a [String],
    live_session_ids: &'a [i64],
    nonterminal_queue_entry_ids: &'a [i64],
    hook_manager: &'a HookManagerState,
    shared_activation_present: bool,
    shared_activation_sha256: &'a Option<String>,
    local_exclude_before_sha256: &'a str,
    local_exclude_after_sha256: &'a str,
    local_exclude_update_required: bool,
    preservation_refs: &'a [EnrollmentPreservationRef],
    safe: bool,
    blockers: &'a [String],
    warnings: &'a [String],
}

struct DisposableRemoteTree {
    _temporary: tempfile::TempDir,
    root: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum EnrollmentPhase {
    Confirmed,
    Preserved,
    RuntimePrepared,
    RemoteMaterialized,
    BasePrepared,
    SessionCreated,
    OutputsApplied,
    Committed,
    Promoted,
    Published,
    Complete,
}

impl EnrollmentPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Preserved => "preserved",
            Self::RuntimePrepared => "runtime_prepared",
            Self::RemoteMaterialized => "remote_materialized",
            Self::BasePrepared => "base_prepared",
            Self::SessionCreated => "session_created",
            Self::OutputsApplied => "outputs_applied",
            Self::Committed => "committed",
            Self::Promoted => "promoted",
            Self::Published => "published",
            Self::Complete => "complete",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EnrollmentJournal {
    schema_version: u32,
    plan_digest: String,
    remote_base: EnrollmentRemoteBase,
    local_default: EnrollmentLocalRef,
    integration: EnrollmentLocalRef,
    generated_plan_digest: String,
    generated_changes: Vec<RepositoryTreeChange>,
    planned_paths: Vec<String>,
    preservation_refs: Vec<EnrollmentPreservationRef>,
    local_exclude_before_sha256: String,
    local_exclude_after_sha256: String,
    local_exclude_update_required: bool,
    phase: EnrollmentPhase,
    bootstrap_session_id: Option<i64>,
    enrollment_session_id: Option<i64>,
    enrollment_commit: Option<String>,
    queue_entry_id: Option<i64>,
    publication_sha: Option<String>,
    verified_remote_sha: Option<String>,
    local_main_synchronized: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FirstEnrollmentExecution {
    pub schema_version: u32,
    pub plan_digest: String,
    pub preservation_refs: Vec<EnrollmentPreservationRef>,
    pub enrollment_session_id: i64,
    pub enrollment_commit: String,
    pub queue_entry_id: i64,
    pub publication_sha: String,
    pub verified_remote_sha: String,
    pub local_main_synchronized: bool,
    pub completed: bool,
    pub journal_path: String,
}

struct EnrollmentLock {
    _file: File,
}

pub fn run(args: &[String]) -> u8 {
    match run_inner(args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("aethyme deploy: {error}");
            1
        }
    }
}

fn run_inner(args: &[String]) -> Result<(), String> {
    let action = args.first().map(String::as_str).unwrap_or("plan");
    if args
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        print_usage();
        return Ok(());
    }
    let mut repo = PathBuf::from(".");
    let mut json = false;
    let mut diff = false;
    let mut confirm = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--repo" => {
                repo = PathBuf::from(args.get(index + 1).ok_or("--repo requires a path")?);
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--diff" => {
                diff = true;
                index += 1;
            }
            "--confirm" => {
                confirm = Some(
                    args.get(index + 1)
                        .ok_or("--confirm requires a plan digest")?
                        .clone(),
                );
                index += 2;
            }
            option => return Err(format!("unknown first-enrollment option {option}")),
        }
    }
    match action {
        "plan" => {
            if confirm.is_some() {
                return Err("deploy plan does not accept --confirm".into());
            }
            let plan = build_plan(&repo)?;
            if diff {
                render_change_inventory(&plan);
            } else if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&plan).map_err(|error| error.to_string())?
                );
            } else {
                render_plan(&plan);
            }
        }
        "execute" => {
            if diff {
                return Err("--diff is available only for deploy plan".into());
            }
            let report = execute(
                &repo,
                confirm
                    .as_deref()
                    .ok_or("deploy execute requires --confirm <plan-sha256>")?,
            )?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
                );
            } else {
                println!("First enrollment published: {}", report.publication_sha);
                println!("Queue entry: {}", report.queue_entry_id);
                println!("Session: {}", report.enrollment_session_id);
                println!(
                    "Local main synchronized: {}",
                    report.local_main_synchronized
                );
                println!("Preservation evidence:");
                for preserved in &report.preservation_refs {
                    println!("  {} -> {}", preserved.ref_name, preserved.target_sha);
                }
                println!("Journal: {}", report.journal_path);
            }
        }
        other => {
            return Err(format!(
                "expected deploy plan or deploy execute, found {other:?}"
            ));
        }
    }
    Ok(())
}

pub fn build_plan(repo_hint: &Path) -> Result<FirstEnrollmentPlan, String> {
    let repo = aethyme_broker::GitRepo::discover(repo_hint).map_err(|error| error.to_string())?;
    let main_root = repo.main_root().map_err(|error| error.to_string())?;
    let remote = "origin";
    let remote_default = repo
        .remote_default_branch(remote)
        .map_err(|error| format!("inspect origin default branch: {error}"))?;
    let default_name = remote_default
        .ref_name
        .strip_prefix("refs/heads/")
        .ok_or_else(|| "origin default branch is not under refs/heads".to_string())?;
    let local_default_ref = format!("refs/heads/{default_name}");
    let integration_branch = aethyme_broker::PromoteConfig::load(&main_root).branch;
    let integration_ref = format!("refs/heads/{integration_branch}");
    let local_default_sha = repo.resolve_ref(&local_default_ref);
    let integration_sha = repo.resolve_ref(&integration_ref);

    let disposable = DisposableRemoteTree::new(&main_root, remote, &remote_default)?;
    let generated_tree =
        crate::repository_upgrade::initial_enrollment_plan(&disposable.root, None)?;
    let planned_paths = generated_tree.planned_paths.clone();
    let generated_changes = generated_tree.changes.clone();
    let dirty_paths = repo.dirty_paths().map_err(|error| error.to_string())?;
    let (overlapping_dirty_paths, disjoint_dirty_paths): (Vec<_>, Vec<_>) =
        dirty_paths.iter().cloned().partition(|dirty| {
            planned_paths
                .iter()
                .any(|planned| paths_overlap(dirty, planned))
        });
    let (live_session_ids, nonterminal_queue_entry_ids) = broker_preconditions(&main_root)?;
    let hook_manager = hook_manager_state(&repo)?;
    let (shared_activation_present, shared_activation_sha256) = activation_state(&repo)?;
    let (local_exclude_before_sha256, local_exclude_after_sha256, local_exclude_update_required) =
        local_exclude_state(&repo)?;
    let (local_ahead_upstream_commits, local_behind_upstream_commits) =
        disposable.relation(local_default_sha.as_deref(), &remote_default.sha)?;

    let integration_can_rebase_to_upstream = integration_sha.is_none()
        || integration_sha.as_deref() == local_default_sha.as_deref()
        || integration_sha.as_deref() == Some(remote_default.sha.as_str());
    let preservation_prefix = format!(
        "aethyme/preservation/enrollment/{}",
        &remote_default.sha[..12]
    );
    let mut preservation_refs = Vec::new();
    if let Some(sha) = &local_default_sha {
        let ref_name = format!("refs/heads/{preservation_prefix}/local-default");
        preservation_refs.push(EnrollmentPreservationRef {
            existing_sha: repo.resolve_ref(&ref_name),
            ref_name,
            target_sha: sha.clone(),
        });
    }
    if let Some(sha) = &integration_sha {
        let ref_name = format!("refs/heads/{preservation_prefix}/integration");
        preservation_refs.push(EnrollmentPreservationRef {
            existing_sha: repo.resolve_ref(&ref_name),
            ref_name,
            target_sha: sha.clone(),
        });
    }

    let mut blockers = generated_tree.blockers.clone();
    if generated_tree.from_schema != 0 {
        blockers.push(
            "the upstream default branch is already enrolled; use deploy verify or repository upgrade instead"
                .into(),
        );
    }
    if planned_paths.is_empty() {
        blockers.push("the upstream default branch has no first-enrollment changes".into());
    }
    if !overlapping_dirty_paths.is_empty() {
        blockers.push(format!(
            "uncommitted paths overlap reviewed enrollment outputs: {}; commit them through the managed pre-commit lane before replanning",
            overlapping_dirty_paths.join(", ")
        ));
    }
    if !live_session_ids.is_empty() {
        blockers.push(format!(
            "live broker sessions must finish before shared enrollment: {}",
            live_session_ids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !nonterminal_queue_entry_ids.is_empty() {
        blockers.push(format!(
            "nonterminal merge queue work requires reconciliation before enrollment: {}",
            nonterminal_queue_entry_ids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if hook_manager.kind == HookManagerKind::Foreign {
        blockers.push(
            "core.hooksPath is owned by another hook manager; Aethyme will not overwrite it — integrate the managed hooks explicitly, then replan"
                .into(),
        );
    }
    if !integration_can_rebase_to_upstream {
        blockers.push(
            "local integration contains work distinct from both local and upstream default branches; run preservation-first integration reconciliation"
                .into(),
        );
    }
    for preservation in &preservation_refs {
        if preservation
            .existing_sha
            .as_ref()
            .is_some_and(|sha| sha != &preservation.target_sha)
        {
            blockers.push(format!(
                "preservation ref {} already names different work",
                preservation.ref_name
            ));
        }
    }
    let mut warnings = Vec::new();
    if !disjoint_dirty_paths.is_empty() {
        warnings.push(
            "uncommitted disjoint paths were not proposal inputs; execution uses an isolated worktree and will not synchronize local main while it is dirty"
                .into(),
        );
    }
    if local_ahead_upstream_commits > 0 {
        warnings.push(format!(
            "local default has {local_ahead_upstream_commits} unpushed commit(s); preservation refs retain them and local-main synchronization will remain disabled if histories diverge"
        ));
    }
    let safe = blockers.is_empty();
    let remote_base = EnrollmentRemoteBase {
        remote: remote.into(),
        default_branch_ref: remote_default.ref_name,
        exact_sha: remote_default.sha,
    };
    let local_default = EnrollmentLocalRef {
        ref_name: local_default_ref,
        exact_sha: local_default_sha,
    };
    let integration = EnrollmentLocalRef {
        ref_name: integration_ref,
        exact_sha: integration_sha,
    };
    let digest = FirstEnrollmentDigest {
        schema_version: PLAN_SCHEMA_VERSION,
        remote_base: &remote_base,
        local_default: &local_default,
        integration: &integration,
        local_ahead_upstream_commits,
        local_behind_upstream_commits,
        integration_can_rebase_to_upstream,
        generated_plan_digest: &generated_tree.plan_digest,
        generated_changes: &generated_changes,
        planned_paths: &planned_paths,
        dirty_paths: &dirty_paths,
        overlapping_dirty_paths: &overlapping_dirty_paths,
        disjoint_dirty_paths: &disjoint_dirty_paths,
        live_session_ids: &live_session_ids,
        nonterminal_queue_entry_ids: &nonterminal_queue_entry_ids,
        hook_manager: &hook_manager,
        shared_activation_present,
        shared_activation_sha256: &shared_activation_sha256,
        local_exclude_before_sha256: &local_exclude_before_sha256,
        local_exclude_after_sha256: &local_exclude_after_sha256,
        local_exclude_update_required,
        preservation_refs: &preservation_refs,
        safe,
        blockers: &blockers,
        warnings: &warnings,
    };
    let plan_digest = sha256(
        &serde_json::to_vec(&digest)
            .map_err(|error| format!("serialize enrollment plan: {error}"))?,
    );
    let next_action = if safe {
        format!("aethyme deploy execute --repo . --confirm {plan_digest}")
    } else {
        "resolve every blocker, then regenerate `aethyme deploy plan --repo .`".into()
    };
    Ok(FirstEnrollmentPlan {
        schema_version: PLAN_SCHEMA_VERSION,
        remote_base,
        local_default,
        integration,
        local_ahead_upstream_commits,
        local_behind_upstream_commits,
        integration_can_rebase_to_upstream,
        generated_tree,
        generated_changes,
        planned_paths,
        dirty_paths,
        overlapping_dirty_paths,
        disjoint_dirty_paths,
        live_session_ids,
        nonterminal_queue_entry_ids,
        hook_manager,
        shared_activation_present,
        shared_activation_sha256,
        local_exclude_before_sha256,
        local_exclude_after_sha256,
        local_exclude_update_required,
        preservation_refs,
        safe,
        blockers,
        warnings,
        plan_digest,
        next_action,
    })
}

pub fn execute(repo_hint: &Path, confirmation: &str) -> Result<FirstEnrollmentExecution, String> {
    if confirmation.len() != 64
        || !confirmation
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err("deploy execute requires the full 64-character plan digest".into());
    }
    let repo = aethyme_broker::GitRepo::discover(repo_hint).map_err(|error| error.to_string())?;
    let main_root = repo.main_root().map_err(|error| error.to_string())?;
    let common_dir = repo.git_common_dir().map_err(|error| error.to_string())?;
    let _lock = acquire_enrollment_lock(&common_dir)?;
    let journal_path = common_dir
        .join("aethyme-broker")
        .join("enrollment-publication.json");
    let mut journal = if journal_path.is_file() {
        let journal = read_journal(&journal_path)?;
        if journal.plan_digest != confirmation {
            return Err(format!(
                "an enrollment execution for plan {} is already in progress; recover it with `aethyme deploy execute --repo . --confirm {}`",
                journal.plan_digest, journal.plan_digest
            ));
        }
        journal
    } else {
        let plan = build_plan(&main_root)?;
        if plan.plan_digest != confirmation {
            return Err(format!(
                "repository or upstream changed after review; expected confirmation {}, received {confirmation}; regenerate `aethyme deploy plan --repo .`",
                plan.plan_digest
            ));
        }
        if !plan.safe {
            return Err(format!(
                "first enrollment is blocked: {}",
                plan.blockers.join("; ")
            ));
        }
        let journal = EnrollmentJournal {
            schema_version: PLAN_SCHEMA_VERSION,
            plan_digest: plan.plan_digest,
            remote_base: plan.remote_base,
            local_default: plan.local_default,
            integration: plan.integration,
            generated_plan_digest: plan.generated_tree.plan_digest,
            generated_changes: plan.generated_changes,
            planned_paths: plan.planned_paths,
            preservation_refs: plan.preservation_refs,
            local_exclude_before_sha256: plan.local_exclude_before_sha256,
            local_exclude_after_sha256: plan.local_exclude_after_sha256,
            local_exclude_update_required: plan.local_exclude_update_required,
            phase: EnrollmentPhase::Confirmed,
            bootstrap_session_id: None,
            enrollment_session_id: None,
            enrollment_commit: None,
            queue_entry_id: None,
            publication_sha: None,
            verified_remote_sha: None,
            local_main_synchronized: false,
        };
        write_journal(&journal_path, &journal)?;
        journal
    };

    if journal.phase == EnrollmentPhase::Complete {
        return execution_report(&journal);
    }
    if journal.phase < EnrollmentPhase::Published {
        let advertised = repo
            .remote_default_branch(&journal.remote_base.remote)
            .map_err(|error| format!("revalidate enrollment upstream: {error}"))?;
        if advertised.ref_name != journal.remote_base.default_branch_ref
            || advertised.sha != journal.remote_base.exact_sha
        {
            return Err(format!(
                "upstream moved after enrollment review; planned {} at {}, now {} at {}. The repository remains preservation-first; inspect `aethyme broker integration reconcile` and never retry publication blindly",
                journal.remote_base.default_branch_ref,
                journal.remote_base.exact_sha,
                advertised.ref_name,
                advertised.sha
            ));
        }
    }

    if journal.phase < EnrollmentPhase::Preserved {
        create_preservation_refs(&repo, &journal.preservation_refs)?;
        journal.phase = EnrollmentPhase::Preserved;
        write_journal(&journal_path, &journal)?;
        maybe_stop_after(journal.phase)?;
    }

    if journal.phase < EnrollmentPhase::RuntimePrepared {
        ensure_local_runtime_exclude(&repo, &journal)?;
        journal.phase = EnrollmentPhase::RuntimePrepared;
        write_journal(&journal_path, &journal)?;
        maybe_stop_after(journal.phase)?;
    }

    if journal.phase < EnrollmentPhase::RemoteMaterialized {
        let mut broker = aethyme_broker::Broker::open(&main_root)
            .map_err(|error| format!("open enrollment broker: {error}"))?;
        let bootstrap = match journal.bootstrap_session_id {
            Some(id) => broker
                .store()
                .session(id)
                .map_err(|error| format!("recover bootstrap session {id}: {error}"))?,
            None => {
                let session = broker
                    .start_worktree("Materialize reviewed first-enrollment upstream")
                    .map_err(|error| format!("start enrollment bootstrap session: {error}"))?;
                journal.bootstrap_session_id = Some(session.id);
                write_journal(&journal_path, &journal)?;
                session
            }
        };
        let default_name = journal
            .remote_base
            .default_branch_ref
            .strip_prefix("refs/heads/")
            .ok_or("reviewed remote default is not under refs/heads")?;
        let tracking_ref = format!("refs/remotes/{}/{default_name}", journal.remote_base.remote);
        let resolved_target = repo
            .resolve_remote_target(&journal.remote_base.remote, None)
            .map_err(|error| format!("resolve enrollment remote: {error}"))?;
        let fetch = broker
            .run_coordinated_operation(aethyme_broker::CoordinatedCommand {
                session_id: bootstrap.id,
                provider: aethyme_broker::OperationProvider::Git,
                repository: None,
                resolved_target: Some(resolved_target),
                scope: Some(format!(
                    "enrollment:fetch:{}",
                    journal.remote_base.default_branch_ref
                )),
                declared_effect: None,
                destructive_confirmed: false,
                authorization_reason: Some(format!(
                    "digest-confirmed first enrollment {}",
                    journal.plan_digest
                )),
                args: vec![
                    "fetch".into(),
                    "--no-tags".into(),
                    "--force".into(),
                    journal.remote_base.remote.clone(),
                    format!("{}:{tracking_ref}", journal.remote_base.default_branch_ref),
                ],
            })
            .map_err(|error| format!("coordinate enrollment fetch: {error}"))?;
        if !fetch.ok() {
            return Err(format!(
                "enrollment fetch operation {} did not succeed; inspect `aethyme broker operations show {}` before recovery",
                fetch.operation.id, fetch.operation.id
            ));
        }
        let finish = broker
            .finish(bootstrap.id)
            .map_err(|error| format!("finish enrollment bootstrap session: {error}"))?;
        if !finish.closed {
            return Err(format!(
                "bootstrap session {} could not finish cleanly; inspect `aethyme broker finish --session {}`",
                bootstrap.id, bootstrap.id
            ));
        }
        if repo.resolve_ref(&journal.remote_base.exact_sha).is_none() {
            return Err(
                "reviewed upstream commit is still unavailable after verified fetch".into(),
            );
        }
        journal.phase = EnrollmentPhase::RemoteMaterialized;
        write_journal(&journal_path, &journal)?;
        maybe_stop_after(journal.phase)?;
    }

    if journal.phase < EnrollmentPhase::BasePrepared {
        align_integration_to_reviewed_upstream(&repo, &journal)?;
        journal.phase = EnrollmentPhase::BasePrepared;
        write_journal(&journal_path, &journal)?;
        maybe_stop_after(journal.phase)?;
    }

    let mut broker = aethyme_broker::Broker::open(&main_root)
        .map_err(|error| format!("open enrollment broker: {error}"))?;
    if journal.phase < EnrollmentPhase::SessionCreated {
        let started = broker
            .start_worktree_with_planned_paths(
                "Publish reviewed Aethyme repository enrollment",
                &journal.planned_paths,
            )
            .map_err(|error| format!("start isolated enrollment session: {error}"))?;
        journal.enrollment_session_id = Some(started.session.id);
        journal.phase = EnrollmentPhase::SessionCreated;
        write_journal(&journal_path, &journal)?;
        maybe_stop_after(journal.phase)?;
    }
    let session_id = journal
        .enrollment_session_id
        .ok_or("enrollment journal lost its session id")?;
    let session = broker
        .store()
        .session(session_id)
        .map_err(|error| format!("recover enrollment session {session_id}: {error}"))?;
    let worktree = PathBuf::from(&session.worktree_path);

    if journal.phase < EnrollmentPhase::OutputsApplied {
        if !outputs_match(&worktree, &journal.generated_changes)? {
            let session_plan =
                crate::repository_upgrade::initial_enrollment_plan(&worktree, Some(session_id))?;
            if session_plan.planned_paths != journal.planned_paths
                || session_plan.changes != journal.generated_changes
            {
                return Err(
                    "generated enrollment tree differs from the reviewed plan; no output was applied"
                        .into(),
                );
            }
            crate::repository_upgrade::apply_initial_enrollment(
                &worktree,
                session_id,
                &session_plan.plan_digest,
            )?;
        }
        aethyme_broker::init::scaffold(&worktree)
            .map_err(|error| format!("activate shared enrollment hooks: {error}"))?;
        let enrollment_repo = aethyme_broker::GitRepo::discover(&worktree)
            .map_err(|error| format!("open enrollment worktree for hooks: {error}"))?;
        let binary = std::env::current_exe()
            .map_err(|error| format!("resolve active Aethyme binary for hooks: {error}"))?;
        aethyme_broker::hooks::install(&enrollment_repo, &binary)
            .map_err(|error| format!("install shared managed hooks: {error}"))?;
        if !outputs_match(&worktree, &journal.generated_changes)? {
            return Err("applied enrollment outputs do not match reviewed hashes".into());
        }
        journal.phase = EnrollmentPhase::OutputsApplied;
        write_journal(&journal_path, &journal)?;
        maybe_stop_after(journal.phase)?;
    }

    if journal.phase < EnrollmentPhase::Committed {
        let commit = commit_enrollment(&worktree, &journal)?;
        journal.enrollment_commit = Some(commit);
        journal.phase = EnrollmentPhase::Committed;
        write_journal(&journal_path, &journal)?;
        maybe_stop_after(journal.phase)?;
    }

    if journal.phase < EnrollmentPhase::Promoted {
        let outcome = broker
            .submit(session_id)
            .map_err(|error| format!("submit reviewed enrollment: {error}"))?;
        if !outcome.conflicts.is_empty() {
            return Err(format!(
                "reviewed enrollment conflicted on {}; inspect the session before recovery",
                outcome.conflicts.join(", ")
            ));
        }
        if !outcome.promoted {
            if outcome.entry.status == aethyme_broker::MergeStatus::Verified {
                broker
                    .promote(outcome.entry.id)
                    .map_err(|error| format!("promote verified enrollment: {error}"))?;
            } else {
                return Err(format!(
                    "enrollment queue entry {} is {}; publication requires a promoted entry",
                    outcome.entry.id,
                    outcome.entry.status.as_str()
                ));
            }
        }
        let ship = broker
            .ship_plan(outcome.entry.id)
            .map_err(|error| format!("plan enrollment publication: {error}"))?;
        journal.queue_entry_id = Some(outcome.entry.id);
        journal.publication_sha = Some(ship.publication_sha);
        journal.phase = EnrollmentPhase::Promoted;
        write_journal(&journal_path, &journal)?;
        maybe_stop_after(journal.phase)?;
    }

    if journal.phase < EnrollmentPhase::Published {
        let entry_id = journal
            .queue_entry_id
            .ok_or("journal lost queue entry id")?;
        let publication_sha = journal
            .publication_sha
            .clone()
            .ok_or("journal lost publication SHA")?;
        let ship_plan = broker
            .ship_plan(entry_id)
            .map_err(|error| format!("revalidate enrollment publication: {error}"))?;
        let synchronize_clean_main = ship_plan.local_main_sync_safe
            && ship_plan
                .local_main_sync_assessment
                .tracked_dirty_paths
                .is_empty()
            && ship_plan
                .local_main_sync_assessment
                .untracked_paths
                .is_empty();
        let shipped = broker
            .ship_execute_with_sync(entry_id, &publication_sha, synchronize_clean_main)
            .map_err(|error| {
                format!(
                    "publish reviewed enrollment: {error}; inspect coordinated operations before rerunning `aethyme deploy execute --repo . --confirm {}`",
                    journal.plan_digest
                )
            })?;
        journal.verified_remote_sha = Some(shipped.verified_remote_sha);
        journal.local_main_synchronized = shipped.local_main_sync.synchronized;
        journal.phase = EnrollmentPhase::Published;
        write_journal(&journal_path, &journal)?;
        maybe_stop_after(journal.phase)?;
    }

    if journal.phase < EnrollmentPhase::Complete {
        let finish = broker
            .finish(session_id)
            .map_err(|error| format!("finish enrollment session: {error}"))?;
        if !finish.closed {
            return Err(format!(
                "enrollment is published, but session {session_id} did not finish; run `aethyme broker finish --session {session_id}`"
            ));
        }
        journal.phase = EnrollmentPhase::Complete;
        write_journal(&journal_path, &journal)?;
    }
    execution_report(&journal)
}

fn execution_report(journal: &EnrollmentJournal) -> Result<FirstEnrollmentExecution, String> {
    Ok(FirstEnrollmentExecution {
        schema_version: PLAN_SCHEMA_VERSION,
        plan_digest: journal.plan_digest.clone(),
        preservation_refs: journal.preservation_refs.clone(),
        enrollment_session_id: journal
            .enrollment_session_id
            .ok_or("completed journal has no enrollment session")?,
        enrollment_commit: journal
            .enrollment_commit
            .clone()
            .ok_or("completed journal has no enrollment commit")?,
        queue_entry_id: journal
            .queue_entry_id
            .ok_or("completed journal has no queue entry")?,
        publication_sha: journal
            .publication_sha
            .clone()
            .ok_or("completed journal has no publication SHA")?,
        verified_remote_sha: journal
            .verified_remote_sha
            .clone()
            .ok_or("completed journal has no verified remote SHA")?,
        local_main_synchronized: journal.local_main_synchronized,
        completed: journal.phase == EnrollmentPhase::Complete,
        journal_path: "git-common-dir/aethyme-broker/enrollment-publication.json".into(),
    })
}

impl DisposableRemoteTree {
    fn new(
        source_repo: &Path,
        remote: &str,
        remote_default: &aethyme_broker::RemoteDefaultBranch,
    ) -> Result<Self, String> {
        let temporary = tempfile::Builder::new()
            .prefix("aethyme-enrollment-plan-")
            .tempdir()
            .map_err(|error| format!("create disposable enrollment plan: {error}"))?;
        let root = temporary.path().join("repository");
        run_git_external(
            source_repo,
            &["clone", "--quiet", "--no-checkout", "--no-hardlinks"],
            Some(&root),
        )?;
        let origin_url = git_output(source_repo, &["remote", "get-url", remote])?;
        run_git(&root, &["remote", "set-url", "origin", &origin_url])?;
        let fetched = Command::new("git")
            .args(["fetch", "--quiet", "origin", &remote_default.ref_name])
            .current_dir(&root)
            .output()
            .map_err(|error| format!("fetch remote enrollment base: {error}"))?;
        if !fetched.status.success() {
            return Err(
                "fetch remote enrollment base failed; credentials and remote output are omitted"
                    .into(),
            );
        }
        run_git(
            &root,
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "checkout",
                "--quiet",
                "--detach",
                &remote_default.sha,
            ],
        )?;
        Ok(Self {
            _temporary: temporary,
            root,
        })
    }

    fn relation(&self, local: Option<&str>, upstream: &str) -> Result<(u64, u64), String> {
        let Some(local) = local else {
            return Ok((0, 0));
        };
        let counts = git_output(
            &self.root,
            &[
                "rev-list",
                "--left-right",
                "--count",
                &format!("{local}...{upstream}"),
            ],
        )?;
        let mut parts = counts.split_whitespace();
        let ahead = parts
            .next()
            .ok_or("missing local-ahead count")?
            .parse::<u64>()
            .map_err(|error| format!("parse local-ahead count: {error}"))?;
        let behind = parts
            .next()
            .ok_or("missing local-behind count")?
            .parse::<u64>()
            .map_err(|error| format!("parse local-behind count: {error}"))?;
        Ok((ahead, behind))
    }
}

fn create_preservation_refs(
    repo: &aethyme_broker::GitRepo,
    refs: &[EnrollmentPreservationRef],
) -> Result<(), String> {
    for preserved in refs {
        let branch = preserved
            .ref_name
            .strip_prefix("refs/heads/")
            .ok_or_else(|| format!("unsafe preservation ref {}", preserved.ref_name))?;
        match repo.resolve_ref(&preserved.ref_name) {
            Some(current) if current == preserved.target_sha => {}
            Some(current) => {
                return Err(format!(
                    "preservation ref {} moved from reviewed {} to {current}; no shared ref was changed",
                    preserved.ref_name, preserved.target_sha
                ));
            }
            None => repo
                .update_branch_ref(branch, &preserved.target_sha)
                .map_err(|error| {
                    format!("create preservation ref {}: {error}", preserved.ref_name)
                })?,
        }
    }
    Ok(())
}

fn align_integration_to_reviewed_upstream(
    repo: &aethyme_broker::GitRepo,
    journal: &EnrollmentJournal,
) -> Result<(), String> {
    let branch = journal
        .integration
        .ref_name
        .strip_prefix("refs/heads/")
        .ok_or("reviewed integration ref is not a local branch")?;
    let target = &journal.remote_base.exact_sha;
    match repo.resolve_ref(&journal.integration.ref_name) {
        Some(current) if current == *target => Ok(()),
        Some(current)
            if Some(current.as_str()) == journal.integration.exact_sha.as_deref()
                || Some(current.as_str()) == journal.local_default.exact_sha.as_deref() =>
        {
            repo.update_branch_ref_checked(branch, target, &current)
                .map_err(|error| format!("prepare reviewed integration base: {error}"))
        }
        Some(current) => Err(format!(
            "integration moved after review from {:?} to {current}; preserve and reconcile it before enrollment",
            journal.integration.exact_sha
        )),
        None if journal.integration.exact_sha.is_none() => repo
            .update_branch_ref(branch, target)
            .map_err(|error| format!("create reviewed integration base: {error}")),
        None => Err("integration disappeared after review; regenerate the enrollment plan".into()),
    }
}

fn commit_enrollment(worktree: &Path, journal: &EnrollmentJournal) -> Result<String, String> {
    let repo = aethyme_broker::GitRepo::discover(worktree).map_err(|error| error.to_string())?;
    let dirty = repo.dirty_paths().map_err(|error| error.to_string())?;
    if dirty.is_empty() {
        let head = repo.head_commit().map_err(|error| error.to_string())?;
        if head == journal.remote_base.exact_sha {
            return Err("reviewed enrollment outputs disappeared before commit".into());
        }
        return Ok(head);
    }
    let unexpected = dirty
        .iter()
        .filter(|path| !journal.planned_paths.iter().any(|planned| planned == *path))
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(format!(
            "enrollment worker contains unreviewed dirty paths: {}",
            unexpected.join(", ")
        ));
    }
    let mut add = Command::new("git");
    add.args(["add", "-f", "-A", "--"])
        .args(&journal.planned_paths)
        .current_dir(worktree);
    let added = add
        .output()
        .map_err(|error| format!("stage reviewed enrollment: {error}"))?;
    if !added.status.success() {
        return Err(format!(
            "stage reviewed enrollment: {}",
            String::from_utf8_lossy(&added.stderr).trim()
        ));
    }
    let committed = Command::new("git")
        .args([
            "commit",
            "-m",
            "chore(aethyme): enroll repository coordination",
        ])
        .current_dir(worktree)
        .output()
        .map_err(|error| format!("commit reviewed enrollment: {error}"))?;
    if !committed.status.success() {
        return Err(format!(
            "commit reviewed enrollment through the managed pre-commit lane: {}",
            String::from_utf8_lossy(&committed.stderr).trim()
        ));
    }
    repo.head_commit().map_err(|error| error.to_string())
}

fn outputs_match(repo: &Path, changes: &[RepositoryTreeChange]) -> Result<bool, String> {
    for change in changes.iter().filter(|change| {
        change.action != crate::repository_upgrade::RepositoryTreeAction::Unchanged
    }) {
        let path = repo.join(&change.path);
        match &change.after_sha256 {
            Some(expected) => {
                let metadata = match std::fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                        metadata
                    }
                    Ok(_) => return Ok(false),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                    Err(error) => return Err(format!("inspect {}: {error}", change.path)),
                };
                let bytes = std::fs::read(&path)
                    .map_err(|error| format!("read {}: {error}", change.path))?;
                if sha256(&bytes) != *expected {
                    return Ok(false);
                }
                #[cfg(unix)]
                if let Some(expected_mode) = change.file_mode.as_deref() {
                    use std::os::unix::fs::PermissionsExt;
                    let actual = if metadata.permissions().mode() & 0o111 == 0 {
                        "100644"
                    } else {
                        "100755"
                    };
                    if actual != expected_mode {
                        return Ok(false);
                    }
                }
            }
            None if path.exists() => return Ok(false),
            None => {}
        }
    }
    Ok(true)
}

fn acquire_enrollment_lock(common_dir: &Path) -> Result<EnrollmentLock, String> {
    let directory = common_dir.join("aethyme-broker");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("create enrollment runtime directory: {error}"))?;
    let path = directory.join("enrollment-publication.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("open enrollment lock: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err("another first-enrollment execution holds the repository lock".into());
        }
    }
    Ok(EnrollmentLock { _file: file })
}

fn read_journal(path: &Path) -> Result<EnrollmentJournal, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("read enrollment journal: {error}"))?;
    let journal: EnrollmentJournal = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse enrollment journal: {error}"))?;
    if journal.schema_version != PLAN_SCHEMA_VERSION {
        return Err(format!(
            "unsupported enrollment journal schema {}",
            journal.schema_version
        ));
    }
    Ok(journal)
}

fn write_journal(path: &Path, journal: &EnrollmentJournal) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create enrollment journal directory: {error}"))?;
    }
    let bytes = serde_json::to_vec_pretty(journal)
        .map_err(|error| format!("serialize enrollment journal: {error}"))?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("create enrollment journal replacement: {error}"))?;
    let result = (|| {
        file.write_all(&bytes)
            .map_err(|error| format!("write enrollment journal: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("sync enrollment journal: {error}"))?;
        std::fs::rename(&temporary, path)
            .map_err(|error| format!("replace enrollment journal: {error}"))?;
        if let Some(parent) = path.parent() {
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("sync enrollment journal directory: {error}"))?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn maybe_stop_after(phase: EnrollmentPhase) -> Result<(), String> {
    if cfg!(debug_assertions)
        && std::env::var("AETHYME_TEST_ENROLLMENT_STOP_AFTER_PHASE")
            .ok()
            .as_deref()
            == Some(phase.as_str())
    {
        return Err(format!(
            "test interruption after persisted enrollment phase {}; resume with the same confirmed plan digest",
            phase.as_str()
        ));
    }
    Ok(())
}

fn broker_preconditions(main_root: &Path) -> Result<(Vec<i64>, Vec<i64>), String> {
    if !main_root.join(aethyme_broker::BROKER_DB_RELPATH).is_file() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut broker = aethyme_broker::Broker::open_snapshot(main_root)
        .map_err(|error| format!("inspect broker preconditions: {error}"))?;
    let mut sessions = broker
        .store()
        .live_sessions()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|session| session.id)
        .collect::<Vec<_>>();
    sessions.sort_unstable();
    let mut entries = broker
        .store()
        .merge_queue()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.status,
                aethyme_broker::MergeStatus::Rejected | aethyme_broker::MergeStatus::Superseded
            )
        })
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    entries.sort_unstable();
    Ok((sessions, entries))
}

fn hook_manager_state(repo: &aethyme_broker::GitRepo) -> Result<HookManagerState, String> {
    let output = Command::new("git")
        .args(["config", "--get", "core.hooksPath"])
        .current_dir(repo.root())
        .output()
        .map_err(|error| format!("inspect core.hooksPath: {error}"))?;
    if output.status.code() == Some(1) {
        return Ok(HookManagerState {
            kind: HookManagerKind::Absent,
            configured_value_sha256: None,
        });
    }
    if !output.status.success() {
        return Err("inspect core.hooksPath failed".into());
    }
    let value = String::from_utf8(output.stdout)
        .map_err(|_| "core.hooksPath is not valid UTF-8".to_string())?
        .trim()
        .to_string();
    let configured = PathBuf::from(&value);
    let resolved = if configured.is_absolute() {
        configured
    } else {
        repo.root().join(configured)
    };
    let default = repo
        .git_common_dir()
        .map_err(|error| error.to_string())?
        .join("hooks");
    let same = match (resolved.canonicalize(), default.canonicalize()) {
        (Ok(resolved), Ok(default)) => resolved == default,
        _ => resolved == default,
    };
    Ok(HookManagerState {
        kind: if same {
            HookManagerKind::AethymeManaged
        } else {
            HookManagerKind::Foreign
        },
        configured_value_sha256: Some(sha256(value.as_bytes())),
    })
}

fn activation_state(repo: &aethyme_broker::GitRepo) -> Result<(bool, Option<String>), String> {
    let path = repo
        .git_common_dir()
        .map_err(|error| error.to_string())?
        .join(aethyme_broker::init::ACTIVATION_MARKER_RELPATH);
    match std::fs::read(path) {
        Ok(bytes) => Ok((true, Some(sha256(&bytes)))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((false, None)),
        Err(error) => Err(format!("inspect shared activation marker: {error}")),
    }
}

fn local_exclude_state(repo: &aethyme_broker::GitRepo) -> Result<(String, String, bool), String> {
    let path = repo
        .git_common_dir()
        .map_err(|error| error.to_string())?
        .join("info/exclude");
    let before = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(format!("inspect local Git exclude: {error}")),
    };
    let after = local_exclude_after(&before);
    Ok((sha256(&before), sha256(&after), before != after))
}

fn local_exclude_after(before: &[u8]) -> Vec<u8> {
    if before
        .windows(RUNTIME_EXCLUDE_BLOCK.len())
        .any(|window| window == RUNTIME_EXCLUDE_BLOCK.as_bytes())
    {
        return before.to_vec();
    }
    let mut after = before.to_vec();
    if !after.is_empty() && !after.ends_with(b"\n") {
        after.push(b'\n');
    }
    if !after.is_empty() && !after.ends_with(b"\n\n") {
        after.push(b'\n');
    }
    after.extend_from_slice(RUNTIME_EXCLUDE_BLOCK.as_bytes());
    after
}

fn ensure_local_runtime_exclude(
    repo: &aethyme_broker::GitRepo,
    journal: &EnrollmentJournal,
) -> Result<(), String> {
    let path = repo
        .git_common_dir()
        .map_err(|error| error.to_string())?
        .join("info/exclude");
    let before = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(format!("revalidate local Git exclude: {error}")),
    };
    if sha256(&before) == journal.local_exclude_after_sha256 {
        return Ok(());
    }
    if sha256(&before) != journal.local_exclude_before_sha256 {
        return Err(
            "local Git exclude changed after review; preservation refs remain intact, regenerate the enrollment plan"
                .into(),
        );
    }
    if !journal.local_exclude_update_required {
        return Ok(());
    }
    let after = local_exclude_after(&before);
    if sha256(&after) != journal.local_exclude_after_sha256 {
        return Err("local Git exclude proposal no longer matches the reviewed digest".into());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create local Git exclude directory: {error}"))?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temporary, &after)
        .map_err(|error| format!("write local Git exclude replacement: {error}"))?;
    std::fs::rename(&temporary, &path)
        .map_err(|error| format!("replace local Git exclude: {error}"))?;
    Ok(())
}

fn paths_overlap(left: &str, right: &str) -> bool {
    left == right
        || (left.ends_with('/') && right.starts_with(left))
        || (right.ends_with('/') && left.starts_with(right))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn git_output(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|error| format!("run git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!("git {} failed", args.join(" ")));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|_| format!("git {} returned non-UTF-8 output", args.join(" ")))
}

fn run_git(repo: &Path, args: &[&str]) -> Result<(), String> {
    git_output(repo, args).map(|_| ())
}

fn run_git_external(
    source: &Path,
    args: &[&str],
    destination: Option<&Path>,
) -> Result<(), String> {
    let mut command = Command::new("git");
    command.args(args).arg(source);
    if let Some(destination) = destination {
        command.arg(destination);
    }
    let status = command
        .status()
        .map_err(|error| format!("materialize enrollment base: {error}"))?;
    if !status.success() {
        return Err("materialize enrollment base failed".into());
    }
    Ok(())
}

fn render_plan(plan: &FirstEnrollmentPlan) {
    println!(
        "First enrollment: {} @ {}",
        plan.remote_base.default_branch_ref,
        &plan.remote_base.exact_sha[..12]
    );
    println!("Plan digest: {}", plan.plan_digest);
    println!("Planned paths: {}", plan.planned_paths.len());
    println!(
        "Local default: {} ahead, {} behind upstream",
        plan.local_ahead_upstream_commits, plan.local_behind_upstream_commits
    );
    println!("Hook manager: {:?}", plan.hook_manager.kind);
    for warning in &plan.warnings {
        println!("Warning: {warning}");
    }
    for blocker in &plan.blockers {
        println!("Blocked: {blocker}");
    }
    println!("Safe: {}", plan.safe);
    println!("Next: {}", plan.next_action);
}

fn render_change_inventory(plan: &FirstEnrollmentPlan) {
    println!(
        "First-enrollment change inventory for {}:",
        plan.remote_base.exact_sha
    );
    for change in &plan.generated_changes {
        println!(
            "  {:?} {} {} -> {} ({:?})",
            change.action,
            change.path,
            change.before_sha256.as_deref().unwrap_or("missing"),
            change.after_sha256.as_deref().unwrap_or("missing"),
            change.ownership,
        );
    }
    println!("Plan digest: {}", plan.plan_digest);
}

fn print_usage() {
    println!("Usage:");
    println!("  aethyme deploy plan [--repo <path>] [--diff|--json]");
    println!("  aethyme deploy execute [--repo <path>] --confirm <plan-sha256> [--json]");
}
