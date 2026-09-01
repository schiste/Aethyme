//! Embedded, versioned migrations for repository deployment artifacts.
//!
//! The binary update and repository update are deliberately separate. A
//! package manager has no trustworthy repository scope; the first broker use
//! in an enrolled repository refuses an old schema and points here instead.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const REPOSITORY_SCHEMA_VERSION: u32 = aethyme_broker::REPOSITORY_SCHEMA_VERSION;
pub const CANONICAL_MARKER_PATH: &str = aethyme_broker::CANONICAL_REPOSITORY_MARKER_PATH;
pub const LOCAL_MARKER_PATH: &str = aethyme_broker::LOCAL_REPOSITORY_MARKER_PATH;
const PLAN_SCHEMA_VERSION: u32 = 5;
const RESOLUTION_SCHEMA_VERSION: u32 = 1;
const GATES_SCHEMA_VERSION: i64 = 1;
const MIGRATION_ID: &str = "repository-deployment-v1";
const MIGRATION_IN_PROGRESS: &str = "repository-deployment-v1:in-progress";
const TRANSACTION_SCHEMA_VERSION: u32 = 1;
const JOURNAL_MAX_BYTES: usize = 64 * 1024 * 1024;
const CRASH_EXIT_CODE: i32 = 86;

#[derive(Debug, Clone, Copy)]
struct EmbeddedMigration {
    id: &'static str,
    from_schema: u32,
    to_schema: u32,
    /// Commands owned by a session pinned to `from_schema` may keep running
    /// while this migration is pending. This is an explicit executable-code
    /// promise, not an inference from the migration being additive.
    backward_executable: bool,
}

const EMBEDDED_MIGRATIONS: &[EmbeddedMigration] = &[EmbeddedMigration {
    id: MIGRATION_ID,
    from_schema: 0,
    to_schema: 1,
    backward_executable: true,
}];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryMode {
    Canonical,
    LocalOnly,
}

impl RepositoryMode {
    fn marker_path(self) -> &'static str {
        match self {
            Self::Canonical => CANONICAL_MARKER_PATH,
            Self::LocalOnly => LOCAL_MARKER_PATH,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryMarker {
    pub schema_version: u32,
    pub applied_migrations: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepositoryUpgradePlan {
    pub schema_version: u32,
    pub mode: RepositoryMode,
    pub from_schema: u32,
    pub to_schema: u32,
    pub repository_head: String,
    pub existing_managed_state_digest: String,
    pub compatibility: CompatibilityDecision,
    pub active_sessions: Vec<UpgradeActiveSessionPrecondition>,
    pub dirty_paths: Vec<String>,
    pub overlapping_dirty_paths: Vec<String>,
    pub disjoint_dirty_paths: Vec<String>,
    pub relevant_leases: Vec<UpgradeRelevantLease>,
    pub shared_policy_or_gate_migration: bool,
    pub warnings: Vec<String>,
    pub migrations: Vec<String>,
    pub changes: Vec<RepositoryTreeChange>,
    pub customizations: Vec<RepositoryCustomization>,
    pub resolution_choices: Vec<RepositoryResolution>,
    pub planned_paths: Vec<String>,
    pub examined_paths: Vec<String>,
    pub diff_sha256: String,
    pub applied: bool,
    pub safe: bool,
    pub blockers: Vec<String>,
    pub plan_digest: String,
    pub next_action: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UpgradeActiveSessionPrecondition {
    pub session_id: i64,
    pub status: aethyme_broker::SessionStatus,
    pub repository_schema: Option<u32>,
    pub deployment_state_digest: Option<String>,
    pub aethyme_version: Option<String>,
    pub gate_definition_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UpgradeRelevantLease {
    pub lease_id: i64,
    pub session_id: i64,
    pub path: String,
    pub kind: aethyme_broker::LeaseKind,
    pub expires_at: Option<i64>,
    pub overlapping_planned_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepositoryUpgradeRecovery {
    pub schema_version: u32,
    pub plan_digest: String,
    pub recovered: bool,
    pub restored_paths: Vec<String>,
    pub next_action: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryResolutionChoice {
    Unresolved,
    Preserve,
    Merge,
    Replace,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RepositoryResolution {
    pub path: String,
    pub choice: RepositoryResolutionChoice,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryCustomizationClassification {
    Missing,
    ManagedBlock,
    KnownGenerated,
    Customized,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RepositoryCustomization {
    pub path: String,
    pub classification: RepositoryCustomizationClassification,
    pub resolution: Option<RepositoryResolutionChoice>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoryResolutionFile {
    schema_version: u32,
    resolutions: BTreeMap<String, RepositoryResolutionChoice>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryTreeAction {
    Create,
    Update,
    Delete,
    Unchanged,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryPathOwnership {
    AethymeOwned,
    ManagedBlock,
    RepositoryOwned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryTreeChange {
    pub path: String,
    pub action: RepositoryTreeAction,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    pub file_mode: Option<String>,
    pub ownership: RepositoryPathOwnership,
    pub requires_resolution: bool,
}

#[derive(Serialize)]
struct PlanDigest<'a> {
    schema_version: u32,
    mode: RepositoryMode,
    from_schema: u32,
    to_schema: u32,
    repository_head: &'a str,
    existing_managed_state_digest: &'a str,
    compatibility: &'a CompatibilityDecision,
    active_sessions: &'a [UpgradeActiveSessionPrecondition],
    dirty_paths: &'a [String],
    overlapping_dirty_paths: &'a [String],
    disjoint_dirty_paths: &'a [String],
    relevant_leases: &'a [UpgradeRelevantLease],
    shared_policy_or_gate_migration: bool,
    warnings: &'a [String],
    migrations: &'a [String],
    changes: &'a [RepositoryTreeChange],
    customizations: &'a [RepositoryCustomization],
    resolution_choices: &'a [RepositoryResolution],
    planned_paths: &'a [String],
    examined_paths: &'a [String],
    diff_sha256: &'a str,
    safe: bool,
    blockers: &'a [String],
}

/// Compatibility of an enrolled repository's deployment artifacts with this
/// binary. This is deliberately separate from broker-storage compatibility:
/// repository upgrades rewrite tracked policy/configuration, while the broker
/// database has its own migration contract.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryCompatibility {
    Current,
    UpgradeRequired,
    NewerThanBinary,
    Invalid,
    UpgradeInProgress,
}

/// The repository authority a parsed command needs.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommandCapability {
    DiagnosticRead,
    RecoveryWrite,
    SessionContinuation,
    ManagedPreCommit,
    NewSession,
    SharedMutation,
    Upgrade,
}

/// User-visible surface that invoked repository compatibility policy.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InvocationSurface {
    Hook,
    #[default]
    BrokerCommand,
    UpgradeCommand,
    CoordinatedOperation,
}

impl InvocationSurface {
    fn refusal_prefix(self) -> &'static str {
        match self {
            Self::Hook => "Aethyme hook refused the operation",
            Self::BrokerCommand => "broker command refused",
            Self::UpgradeCommand => "upgrade command refused",
            Self::CoordinatedOperation => "coordinated operation refused",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilitySeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityExecution {
    Normal,
    ReadOnlySnapshot,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CompatibilityContext<'a> {
    pub session_contract: Option<&'a aethyme_broker::RepositoryContract>,
    pub surface: InvocationSurface,
}

/// A pure, render-independent compatibility decision for one parsed command.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CompatibilityDecision {
    pub repository: RepositoryCompatibility,
    pub repository_schema: Option<u32>,
    pub capability: CommandCapability,
    pub surface: InvocationSurface,
    pub allowed: bool,
    pub severity: CompatibilitySeverity,
    pub execution: CompatibilityExecution,
    pub reason: String,
    pub remediation: Option<String>,
}

impl CompatibilityDecision {
    pub fn refusal_message(&self) -> Option<String> {
        if self.allowed {
            return None;
        }
        let refusal = format!("{}: {}", self.surface.refusal_prefix(), self.reason);
        Some(match &self.remediation {
            Some(remediation) => format!("{refusal}; {remediation}"),
            None => refusal,
        })
    }

    /// The managed pre-commit hook is the last guard before Git writes a
    /// commit. Its refusal must make clear that compatibility—not the staged
    /// changes—caused the stop, and that the worktree was left untouched.
    pub fn managed_pre_commit_refusal_message(&self) -> Option<String> {
        if self.allowed || self.capability != CommandCapability::ManagedPreCommit {
            return None;
        }
        let repository_schema = self.repository_schema?;
        (repository_schema < REPOSITORY_SCHEMA_VERSION).then(|| {
            format!(
                "git commit refused by Aethyme pre-commit:\n\
                 repository deployment schema {repository_schema} must be upgraded to schema \
                 {REPOSITORY_SCHEMA_VERSION}.\n\
                 Your changes remain in the worktree."
            )
        })
    }
}

/// Inspect an enrolled repository and decide whether a parsed command may run.
/// Unenrolled directories return `None`, preserving the setup/first-run path.
pub fn compatibility_decision(
    repo_hint: &Path,
    capability: CommandCapability,
    context: CompatibilityContext<'_>,
) -> Option<CompatibilityDecision> {
    let repo = git_root(repo_hint).ok()?;
    let mode = detect_mode(&repo)?;
    let (repository, repository_schema, reason, remediation) = match read_marker(&repo, mode) {
        Ok(marker) => {
            let repository = classify_marker(marker.as_ref());
            let repository_schema = marker.as_ref().map_or(0, |marker| marker.schema_version);
            let (reason, remediation) = match repository {
                RepositoryCompatibility::Current => {
                    ("repository deployment is current".into(), None)
                }
                RepositoryCompatibility::NewerThanBinary => {
                    let marker = marker.as_ref().expect("newer state requires a marker");
                    (
                        format!(
                            "repository schema {} is newer than this binary supports ({})",
                            marker.schema_version, REPOSITORY_SCHEMA_VERSION
                        ),
                        Some("update Aethyme before using broker commands".into()),
                    )
                }
                RepositoryCompatibility::UpgradeRequired
                | RepositoryCompatibility::UpgradeInProgress => (
                    "repository deployment requires an embedded upgrade".into(),
                    Some(
                        "run `aethyme upgrade plan --repo .`, review it, then `aethyme upgrade apply --repo . --confirm <plan-sha256>`"
                            .into(),
                    ),
                ),
                RepositoryCompatibility::Invalid => {
                    unreachable!("marker parsing errors are handled separately")
                }
            };
            (repository, Some(repository_schema), reason, remediation)
        }
        Err(error) => (
            RepositoryCompatibility::Invalid,
            None,
            format!("repository deployment marker is invalid: {error}"),
            Some(format!(
                "inspect {} and run `aethyme upgrade plan --repo .`",
                mode.marker_path()
            )),
        ),
    };
    Some(decide_compatibility(
        repository,
        repository_schema,
        capability,
        context,
        reason,
        remediation,
    ))
}

fn decide_compatibility(
    repository: RepositoryCompatibility,
    repository_schema: Option<u32>,
    capability: CommandCapability,
    context: CompatibilityContext<'_>,
    reason: String,
    remediation: Option<String>,
) -> CompatibilityDecision {
    let pinned_session_is_executable = repository_schema.is_some_and(|repository_schema| {
        context.session_contract.is_some_and(|contract| {
            contract.repository_schema.unwrap_or(0) == repository_schema
                && migration_path_is_backward_executable(repository_schema)
        })
    });
    let allowed = match repository {
        RepositoryCompatibility::Current => true,
        RepositoryCompatibility::UpgradeRequired | RepositoryCompatibility::UpgradeInProgress => {
            match capability {
                CommandCapability::DiagnosticRead
                | CommandCapability::RecoveryWrite
                | CommandCapability::Upgrade => true,
                CommandCapability::SessionContinuation | CommandCapability::ManagedPreCommit => {
                    pinned_session_is_executable
                }
                CommandCapability::NewSession | CommandCapability::SharedMutation => false,
            }
        }
        RepositoryCompatibility::NewerThanBinary | RepositoryCompatibility::Invalid => matches!(
            capability,
            CommandCapability::DiagnosticRead | CommandCapability::Upgrade
        ),
    };
    let execution = if allowed
        && repository != RepositoryCompatibility::Current
        && capability == CommandCapability::DiagnosticRead
    {
        CompatibilityExecution::ReadOnlySnapshot
    } else {
        CompatibilityExecution::Normal
    };
    let severity = match repository {
        RepositoryCompatibility::Current => CompatibilitySeverity::Info,
        RepositoryCompatibility::UpgradeRequired | RepositoryCompatibility::UpgradeInProgress => {
            CompatibilitySeverity::Warning
        }
        RepositoryCompatibility::NewerThanBinary | RepositoryCompatibility::Invalid => {
            CompatibilitySeverity::Error
        }
    };
    let remediation = contextual_remediation(repository, context.surface, remediation);
    CompatibilityDecision {
        repository,
        repository_schema,
        capability,
        surface: context.surface,
        allowed,
        severity,
        execution,
        reason,
        remediation,
    }
}

fn contextual_remediation(
    repository: RepositoryCompatibility,
    surface: InvocationSurface,
    fallback: Option<String>,
) -> Option<String> {
    match repository {
        RepositoryCompatibility::UpgradeRequired | RepositoryCompatibility::UpgradeInProgress => {
            Some(match surface {
                InvocationSurface::Hook => {
                    "finish or upgrade the accepted session before retrying the hook".into()
                }
                InvocationSurface::BrokerCommand | InvocationSurface::CoordinatedOperation => {
                    "commit pending work through the managed pre-commit lane or finish active sessions with `aethyme broker finish --session <id>`; then run `aethyme upgrade plan --repo .`, review it, and apply it with `aethyme upgrade apply --repo . --confirm <plan-sha256>`".into()
                }
                InvocationSurface::UpgradeCommand => fallback.unwrap_or_else(|| {
                    "review the upgrade plan and apply it with its exact digest".into()
                }),
            })
        }
        RepositoryCompatibility::NewerThanBinary => Some(match surface {
            InvocationSurface::CoordinatedOperation => {
                "update Aethyme before retrying the coordinated operation".into()
            }
            InvocationSurface::Hook => "update Aethyme before retrying git commit".into(),
            InvocationSurface::BrokerCommand => {
                "update Aethyme before retrying the broker command".into()
            }
            InvocationSurface::UpgradeCommand => {
                "update Aethyme before planning a repository downgrade".into()
            }
        }),
        RepositoryCompatibility::Current | RepositoryCompatibility::Invalid => fallback,
    }
}

fn migration_path_is_backward_executable(from_schema: u32) -> bool {
    let mut cursor = from_schema;
    let mut found = false;
    for migration in EMBEDDED_MIGRATIONS {
        if migration.from_schema != cursor {
            continue;
        }
        found = true;
        if !migration.backward_executable {
            return false;
        }
        cursor = migration.to_schema;
        if cursor == REPOSITORY_SCHEMA_VERSION {
            return true;
        }
    }
    found && cursor == REPOSITORY_SCHEMA_VERSION
}

pub fn write_current_marker(repo: &Path, mode: RepositoryMode) -> Result<(), String> {
    write_marker(
        repo,
        mode,
        &RepositoryMarker {
            schema_version: REPOSITORY_SCHEMA_VERSION,
            applied_migrations: vec![MIGRATION_ID.into()],
        },
    )
}

fn write_pending_marker(repo: &Path, mode: RepositoryMode) -> Result<(), String> {
    write_marker(
        repo,
        mode,
        &RepositoryMarker {
            schema_version: 0,
            applied_migrations: vec![MIGRATION_IN_PROGRESS.into()],
        },
    )
}

fn write_marker(
    repo: &Path,
    mode: RepositoryMode,
    marker: &RepositoryMarker,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(&marker).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    atomic_write(&repo.join(mode.marker_path()), &bytes)
}

pub fn verify_current_marker(repo: &Path, mode: RepositoryMode) -> Result<(), String> {
    let marker = read_marker(repo, mode)?.ok_or_else(|| {
        format!(
            "missing {}; run `aethyme upgrade plan --repo .`",
            mode.marker_path()
        )
    })?;
    if marker.schema_version != REPOSITORY_SCHEMA_VERSION {
        return Err(format!(
            "repository schema is {}, binary requires {}; run `aethyme upgrade plan --repo .`",
            marker.schema_version, REPOSITORY_SCHEMA_VERSION
        ));
    }
    if !marker
        .applied_migrations
        .iter()
        .any(|migration| migration == MIGRATION_ID)
    {
        return Err(format!(
            "{} does not record required migration {MIGRATION_ID}; run `aethyme upgrade plan --repo .`",
            mode.marker_path()
        ));
    }
    Ok(())
}

fn marker_is_current(marker: &RepositoryMarker) -> bool {
    marker.schema_version == REPOSITORY_SCHEMA_VERSION
        && marker
            .applied_migrations
            .iter()
            .any(|migration| migration == MIGRATION_ID)
}

fn classify_marker(marker: Option<&RepositoryMarker>) -> RepositoryCompatibility {
    match marker {
        Some(marker) if marker_is_current(marker) => RepositoryCompatibility::Current,
        Some(marker) if marker.schema_version > REPOSITORY_SCHEMA_VERSION => {
            RepositoryCompatibility::NewerThanBinary
        }
        Some(marker) if marker_upgrade_is_in_progress(marker) => {
            RepositoryCompatibility::UpgradeInProgress
        }
        Some(_) | None => RepositoryCompatibility::UpgradeRequired,
    }
}

fn marker_upgrade_is_in_progress(marker: &RepositoryMarker) -> bool {
    marker
        .applied_migrations
        .iter()
        .any(|migration| migration == MIGRATION_IN_PROGRESS)
}

fn read_marker(repo: &Path, mode: RepositoryMode) -> Result<Option<RepositoryMarker>, String> {
    if let Some(blocker) = managed_path_blocker(repo, mode.marker_path())? {
        return Err(blocker);
    }
    let path = repo.join(mode.marker_path());
    if !path.exists() {
        return Ok(None);
    }
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("{}: {error}", mode.marker_path()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{} must be a regular file", mode.marker_path()));
    }
    let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        format!(
            "{} is not a valid repository marker: {error}",
            mode.marker_path()
        )
    })
}

struct ProposedRepository {
    _temporary: tempfile::TempDir,
    root: PathBuf,
}

struct BuiltUpgradePlan {
    report: RepositoryUpgradePlan,
    migration_diff: String,
    proposed_outputs: Vec<ProposedOutput>,
}

#[derive(Clone)]
struct ProposedOutput {
    path: String,
    action: RepositoryTreeAction,
    bytes: Option<Vec<u8>>,
    file_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpgradeRollbackJournal {
    schema_version: u32,
    plan_digest: String,
    repository_head: String,
    existing_managed_state_digest: String,
    mode: RepositoryMode,
    marker_path: String,
    entries: Vec<UpgradeRollbackEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpgradeRollbackEntry {
    path: String,
    action: RepositoryTreeAction,
    before: Option<UpgradeBackup>,
    after_sha256: Option<String>,
    after_mode: Option<String>,
    replacement_path: Option<String>,
    tombstone_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpgradeBackup {
    sha256: String,
    file_mode: String,
    bytes: Vec<u8>,
}

struct UpgradeLock {
    file: File,
}

#[cfg(unix)]
impl Drop for UpgradeLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl ProposedRepository {
    fn from_committed_head(repo: &Path, head: &str) -> Result<Self, String> {
        let temporary = tempfile::Builder::new()
            .prefix("aethyme-upgrade-proposal-")
            .tempdir()
            .map_err(|error| format!("create disposable upgrade directory: {error}"))?;
        let root = temporary.path().join("repository");
        let cloned = Command::new("git")
            .arg("clone")
            .args(["--quiet", "--no-checkout", "--no-hardlinks"])
            .arg(repo)
            .arg(&root)
            .output()
            .map_err(|error| format!("materialize committed repository: {error}"))?;
        if !cloned.status.success() {
            return Err(format!(
                "materialize committed repository: {}",
                String::from_utf8_lossy(&cloned.stderr).trim()
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

        // A local clone rewrites origin to the source checkout path. Preserve
        // the source repository's configured origin so generators derive the
        // same canonical repository identity without reading worktree files.
        if let Ok(origin) = git(repo, &["remote", "get-url", "origin"])
            && !origin.is_empty()
        {
            git(&root, &["remote", "set-url", "origin", &origin])?;
        }
        Ok(Self {
            _temporary: temporary,
            root,
        })
    }

    fn root(&self) -> &Path {
        &self.root
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeEntry {
    sha256: String,
    file_mode: String,
}

fn committed_paths(repo: &Path, head: &str) -> Result<Vec<String>, String> {
    let output = git_bytes(repo, &["ls-tree", "-r", "--name-only", "-z", head])?;
    utf8_paths_z(&output)
}

fn proposed_changed_paths(repo: &Path) -> Result<Vec<String>, String> {
    let mut paths = BTreeSet::new();
    for args in [
        &["diff", "--name-only", "-z", "HEAD"][..],
        &["diff", "--cached", "--name-only", "-z", "HEAD"][..],
        &["ls-files", "--others", "--exclude-standard", "-z"][..],
    ] {
        paths.extend(utf8_paths_z(&git_bytes(repo, args)?)?);
    }
    Ok(paths.into_iter().collect())
}

fn utf8_paths_z(output: &[u8]) -> Result<Vec<String>, String> {
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            std::str::from_utf8(path)
                .map(str::to_string)
                .map_err(|_| "repository upgrade paths must be valid UTF-8".to_string())
        })
        .collect()
}

fn snapshot_paths(
    repo: &Path,
    paths: &[String],
) -> Result<BTreeMap<String, Option<TreeEntry>>, String> {
    paths
        .iter()
        .map(|relative| {
            snapshot_entry(&repo.join(relative))
                .map(|entry| (relative.clone(), entry))
                .map_err(|error| format!("inspect proposed path {relative}: {error}"))
        })
        .collect()
}

fn snapshot_entry(path: &Path) -> Result<Option<TreeEntry>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let (bytes, file_mode) = if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(path).map_err(|error| error.to_string())?;
        (path_bytes(&target), "120000".to_string())
    } else if metadata.is_file() {
        (
            std::fs::read(path).map_err(|error| error.to_string())?,
            regular_file_mode(&metadata),
        )
    } else {
        return Ok(None);
    };
    Ok(Some(TreeEntry {
        sha256: sha256(&bytes),
        file_mode,
    }))
}

fn classify_tree_changes(
    mode: RepositoryMode,
    paths: &[String],
    before: &BTreeMap<String, Option<TreeEntry>>,
    after: &BTreeMap<String, Option<TreeEntry>>,
    resolution_paths: &BTreeSet<String>,
) -> Vec<RepositoryTreeChange> {
    paths
        .iter()
        .map(|path| {
            let before = before.get(path).and_then(Option::as_ref);
            let after = after.get(path).and_then(Option::as_ref);
            let action = match (before, after) {
                (None, None) => RepositoryTreeAction::Unchanged,
                (Some(before), Some(after)) if before == after => RepositoryTreeAction::Unchanged,
                (None, Some(_)) => RepositoryTreeAction::Create,
                (Some(_), None) => RepositoryTreeAction::Delete,
                (Some(_), Some(_)) => RepositoryTreeAction::Update,
            };
            let ownership = path_ownership(mode, path);
            let requires_resolution = resolution_paths.contains(path)
                || (action != RepositoryTreeAction::Unchanged
                    && ownership == RepositoryPathOwnership::RepositoryOwned
                    && before.is_some());
            RepositoryTreeChange {
                path: path.clone(),
                action,
                before_sha256: before.map(|entry| entry.sha256.clone()),
                after_sha256: after.map(|entry| entry.sha256.clone()),
                file_mode: after.or(before).map(|entry| entry.file_mode.clone()),
                ownership,
                requires_resolution,
            }
        })
        .collect()
}

fn path_ownership(_mode: RepositoryMode, path: &str) -> RepositoryPathOwnership {
    if matches!(
        path,
        ".gitignore" | "AGENTS.md" | "CLAUDE.md" | ".claude/settings.local.json"
    ) {
        RepositoryPathOwnership::ManagedBlock
    } else if matches!(
        path,
        ".aethyme/config.toml" | ".aethyme/gates.toml" | ".aethyme/overrides/agents.md"
    ) {
        RepositoryPathOwnership::RepositoryOwned
    } else {
        RepositoryPathOwnership::AethymeOwned
    }
}

#[cfg(unix)]
fn regular_file_mode(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o111 == 0 {
        "100644".into()
    } else {
        "100755".into()
    }
}

#[cfg(not(unix))]
fn regular_file_mode(_metadata: &std::fs::Metadata) -> String {
    "100644".into()
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().as_bytes().to_vec()
}

fn load_resolution_file(
    repo: &Path,
    resolution_file: Option<&Path>,
) -> Result<BTreeMap<String, RepositoryResolutionChoice>, String> {
    let Some(resolution_file) = resolution_file else {
        return Ok(BTreeMap::new());
    };
    let path = if resolution_file.is_absolute() {
        resolution_file.to_path_buf()
    } else {
        repo.join(resolution_file)
    };
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("read resolution file {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("upgrade resolution file must be a regular non-symlink file".into());
    }
    if metadata.len() > 1024 * 1024 {
        return Err("upgrade resolution file exceeds the 1 MiB limit".into());
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("read resolution file {}: {error}", path.display()))?;
    let parsed: RepositoryResolutionFile = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid upgrade resolution file: {error}"))?;
    if parsed.schema_version != RESOLUTION_SCHEMA_VERSION {
        return Err(format!(
            "unsupported upgrade resolution schema {}; expected {RESOLUTION_SCHEMA_VERSION}",
            parsed.schema_version
        ));
    }
    if parsed
        .resolutions
        .values()
        .any(|choice| *choice == RepositoryResolutionChoice::Unresolved)
    {
        return Err("resolution files must choose preserve, merge, or replace".into());
    }
    Ok(parsed.resolutions)
}

fn marked_policy(content: &str) -> String {
    format!(
        "{}\n{}\n{}",
        aethyme_enhance::BLOCK_BEGIN,
        content.trim_end(),
        aethyme_enhance::BLOCK_END
    )
}

fn classify_policy(
    repo: &Path,
    relative: &str,
) -> Result<RepositoryCustomizationClassification, String> {
    let path = repo.join(relative);
    if !path.exists() {
        return Ok(RepositoryCustomizationClassification::Missing);
    }
    let existing = std::fs::read_to_string(&path)
        .map_err(|error| format!("inspect policy {relative}: {error}"))?;
    let begin_count = existing.matches(aethyme_enhance::BLOCK_BEGIN).count();
    let end_count = existing.matches(aethyme_enhance::BLOCK_END).count();
    let ordered_markers = existing
        .find(aethyme_enhance::BLOCK_BEGIN)
        .zip(existing.find(aethyme_enhance::BLOCK_END))
        .map(|(begin, end)| begin < end)
        .unwrap_or(false);
    if begin_count == 1 && end_count == 1 && ordered_markers {
        return Ok(RepositoryCustomizationClassification::ManagedBlock);
    }
    let current = aethyme_enhance::agents::render_agents_document(Some(repo))?;
    let base = aethyme_enhance::agents::render_agents_document(None)?;
    if [
        current.as_str(),
        base.as_str(),
        aethyme_enhance::templates::AGENTS_MD,
    ]
    .iter()
    .any(|known| existing == *known)
    {
        Ok(RepositoryCustomizationClassification::KnownGenerated)
    } else {
        Ok(RepositoryCustomizationClassification::Customized)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatesDocumentVersion {
    Legacy,
    V1,
}

fn gates_document_version(
    document: &toml_edit::DocumentMut,
) -> Result<GatesDocumentVersion, String> {
    match document.get("schema") {
        None => Ok(GatesDocumentVersion::Legacy),
        Some(schema) if schema.as_integer() == Some(GATES_SCHEMA_VERSION) => {
            Ok(GatesDocumentVersion::V1)
        }
        Some(schema) => Err(format!(
            "gates.toml schema must be {GATES_SCHEMA_VERSION}, found {schema}"
        )),
    }
}

fn migrate_gates_text(text: &str) -> Result<String, String> {
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("gates.toml is not valid TOML: {error}"))?;
    match gates_document_version(&document)? {
        GatesDocumentVersion::Legacy => {
            document["schema"] = toml_edit::value(GATES_SCHEMA_VERSION);
        }
        GatesDocumentVersion::V1 => {}
    }
    Ok(document.to_string())
}

fn classify_gates(repo: &Path) -> Result<RepositoryCustomizationClassification, String> {
    let path = repo.join(aethyme_broker::GATES_CONFIG_RELPATH);
    if !path.exists() {
        return Ok(RepositoryCustomizationClassification::Missing);
    }
    let existing = std::fs::read_to_string(&path)
        .map_err(|error| format!("inspect .aethyme/gates.toml: {error}"))?;
    let Some(legacy_draft) = aethyme_broker::init::draft_gate_config(repo) else {
        return Ok(RepositoryCustomizationClassification::Customized);
    };
    let current_draft = migrate_gates_text(&legacy_draft)?;
    if existing == legacy_draft || existing == current_draft {
        Ok(RepositoryCustomizationClassification::KnownGenerated)
    } else {
        Ok(RepositoryCustomizationClassification::Customized)
    }
}

fn customization_paths(mode: RepositoryMode) -> Vec<&'static str> {
    match mode {
        RepositoryMode::Canonical => {
            vec![
                "AGENTS.md",
                "CLAUDE.md",
                aethyme_broker::GATES_CONFIG_RELPATH,
            ]
        }
        RepositoryMode::LocalOnly => vec![
            aethyme_enhance::local::LOCAL_POLICY_PATH,
            aethyme_broker::GATES_CONFIG_RELPATH,
        ],
    }
}

fn assess_customizations(
    repo: &Path,
    mode: RepositoryMode,
    requested: &BTreeMap<String, RepositoryResolutionChoice>,
    blocked_paths: &BTreeSet<String>,
) -> Result<
    (
        Vec<RepositoryCustomization>,
        Vec<RepositoryResolution>,
        Vec<String>,
    ),
    String,
> {
    let mut customizations = Vec::new();
    for relative in customization_paths(mode) {
        let classification = if blocked_paths.contains(relative) {
            RepositoryCustomizationClassification::Customized
        } else if relative == aethyme_broker::GATES_CONFIG_RELPATH {
            classify_gates(repo)?
        } else {
            classify_policy(repo, relative)?
        };
        let resolution = (classification == RepositoryCustomizationClassification::Customized)
            .then(|| requested.get(relative).copied())
            .flatten();
        customizations.push(RepositoryCustomization {
            path: relative.to_string(),
            classification,
            resolution,
        });
    }
    for path in requested.keys() {
        let Some(customization) = customizations.iter().find(|item| item.path == *path) else {
            return Err(format!(
                "resolution file names unmanaged upgrade path {path}"
            ));
        };
        if customization.classification != RepositoryCustomizationClassification::Customized {
            return Err(format!(
                "resolution for {} is unnecessary because it is classified as {:?}",
                customization.path, customization.classification
            ));
        }
    }
    let mut blockers = Vec::new();
    let resolutions = customizations
        .iter()
        .filter(|item| item.classification == RepositoryCustomizationClassification::Customized)
        .map(|item| {
            let choice = item
                .resolution
                .unwrap_or(RepositoryResolutionChoice::Unresolved);
            if choice == RepositoryResolutionChoice::Unresolved {
                blockers.push(format!(
                    "customized policy {} requires an explicit preserve, merge, or replace resolution",
                    item.path
                ));
            }
            RepositoryResolution {
                path: item.path.clone(),
                choice,
            }
        })
        .collect::<Vec<_>>();
    for customization in &customizations {
        if customization.classification != RepositoryCustomizationClassification::Customized {
            continue;
        }
        match customization.resolution {
            Some(RepositoryResolutionChoice::Merge)
                if customization.path == aethyme_broker::GATES_CONFIG_RELPATH =>
            {
                let source = std::fs::read_to_string(repo.join(&customization.path))
                    .map_err(|error| format!("inspect {}: {error}", customization.path))?;
                if let Err(error) = migrate_gates_text(&source) {
                    blockers.push(format!(
                        "cannot merge customized {}: {error}; choose preserve or replace",
                        customization.path
                    ));
                }
            }
            Some(RepositoryResolutionChoice::Merge) => {
                let source = std::fs::read_to_string(repo.join(&customization.path))
                    .map_err(|error| format!("inspect {}: {error}", customization.path))?;
                if source.contains(aethyme_enhance::BLOCK_BEGIN)
                    || source.contains(aethyme_enhance::BLOCK_END)
                {
                    blockers.push(format!(
                        "cannot merge malformed managed markers in {}; choose preserve or replace",
                        customization.path
                    ));
                }
            }
            Some(RepositoryResolutionChoice::Replace)
                if customization.path == aethyme_broker::GATES_CONFIG_RELPATH
                    && aethyme_broker::init::draft_gate_config(repo).is_none() =>
            {
                blockers.push(format!(
                    "cannot replace {} because no supported manifest produces a gate draft; choose preserve or merge",
                    customization.path
                ));
            }
            _ => {}
        }
    }
    Ok((customizations, resolutions, blockers))
}

fn resolved_choice(
    customizations: &[RepositoryCustomization],
    path: &str,
) -> Option<RepositoryResolutionChoice> {
    customizations
        .iter()
        .find(|item| item.path == path)
        .and_then(|item| item.resolution)
}

fn migrate_policy_file(
    repo: &Path,
    relative: &str,
    classification: RepositoryCustomizationClassification,
    resolution: Option<RepositoryResolutionChoice>,
) -> Result<(), String> {
    let path = repo.join(relative);
    let generated = aethyme_enhance::agents::render_agents_document(Some(repo))?;
    let managed = marked_policy(&generated);
    let existing = match std::fs::read_to_string(&path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("read policy {relative}: {error}")),
    };
    let updated = match classification {
        RepositoryCustomizationClassification::Missing
        | RepositoryCustomizationClassification::KnownGenerated => format!("{managed}\n"),
        RepositoryCustomizationClassification::ManagedBlock => {
            aethyme_enhance::render::splice_generated_block(&existing, &managed)
        }
        RepositoryCustomizationClassification::Customized => match resolution {
            Some(RepositoryResolutionChoice::Preserve) => return Ok(()),
            Some(RepositoryResolutionChoice::Merge) => {
                if existing.contains(aethyme_enhance::BLOCK_BEGIN)
                    || existing.contains(aethyme_enhance::BLOCK_END)
                {
                    return Err(format!(
                        "cannot merge malformed managed markers in {relative}; choose preserve or replace"
                    ));
                }
                let mut merged = existing.trim_end().to_string();
                if !merged.is_empty() {
                    merged.push_str("\n\n");
                }
                merged.push_str(&managed);
                merged.push('\n');
                merged
            }
            Some(RepositoryResolutionChoice::Replace) => format!("{managed}\n"),
            Some(RepositoryResolutionChoice::Unresolved) | None => {
                return Err(format!("unresolved customized policy {relative}"));
            }
        },
    };
    atomic_write(&path, updated.as_bytes())
}

fn migrate_gates_file(
    repo: &Path,
    classification: RepositoryCustomizationClassification,
    resolution: Option<RepositoryResolutionChoice>,
) -> Result<(), String> {
    let path = repo.join(aethyme_broker::GATES_CONFIG_RELPATH);
    let source = match classification {
        RepositoryCustomizationClassification::Missing => {
            let Some(draft) = aethyme_broker::init::draft_gate_config(repo) else {
                return Ok(());
            };
            draft
        }
        RepositoryCustomizationClassification::KnownGenerated => std::fs::read_to_string(&path)
            .map_err(|error| format!("read .aethyme/gates.toml: {error}"))?,
        RepositoryCustomizationClassification::Customized => match resolution {
            Some(RepositoryResolutionChoice::Preserve) => return Ok(()),
            Some(RepositoryResolutionChoice::Merge) => std::fs::read_to_string(&path)
                .map_err(|error| format!("read .aethyme/gates.toml: {error}"))?,
            Some(RepositoryResolutionChoice::Replace) => {
                aethyme_broker::init::draft_gate_config(repo).ok_or_else(|| {
                    "cannot replace gates.toml because no supported manifests produce a gate draft"
                        .to_string()
                })?
            }
            Some(RepositoryResolutionChoice::Unresolved) | None => {
                return Err("unresolved customized policy .aethyme/gates.toml".into());
            }
        },
        RepositoryCustomizationClassification::ManagedBlock => unreachable!(),
    };
    let migrated = migrate_gates_text(&source)?;
    atomic_write(&path, migrated.as_bytes())?;
    aethyme_broker::load_gates(repo)
        .map(|_| ())
        .map_err(|error| format!("migrated gates.toml is invalid: {error}"))
}

fn apply_customization_migrations(
    repo: &Path,
    mode: RepositoryMode,
    customizations: &[RepositoryCustomization],
) -> Result<(), String> {
    let gates = customizations
        .iter()
        .find(|item| item.path == aethyme_broker::GATES_CONFIG_RELPATH)
        .expect("gates customization is always assessed");
    migrate_gates_file(repo, gates.classification, gates.resolution)?;
    match mode {
        RepositoryMode::Canonical => {
            aethyme_enhance::deploy::deploy_supporting_artifacts(repo, true)?;
        }
        RepositoryMode::LocalOnly => {
            aethyme_enhance::local::deploy_supporting_artifacts(repo, true)?;
        }
    }
    for customization in customizations {
        if customization.path == aethyme_broker::GATES_CONFIG_RELPATH {
            continue;
        }
        migrate_policy_file(
            repo,
            &customization.path,
            customization.classification,
            resolved_choice(customizations, &customization.path),
        )?;
    }
    Ok(())
}

fn render_migration_diff(repo: &Path, planned_paths: &[String]) -> Result<String, String> {
    if planned_paths.is_empty() {
        return Ok(String::new());
    }
    let staged = Command::new("git")
        .args(["-c", "core.hooksPath=/dev/null", "add", "-f", "-A", "--"])
        .args(planned_paths)
        .current_dir(repo)
        .output()
        .map_err(|error| format!("stage proposed migration tree: {error}"))?;
    if !staged.status.success() {
        return Err(format!(
            "stage proposed migration tree: {}",
            String::from_utf8_lossy(&staged.stderr).trim()
        ));
    }
    let rendered = Command::new("git")
        .args([
            "-c",
            "color.ui=false",
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "HEAD",
            "--",
        ])
        .args(planned_paths)
        .current_dir(repo)
        .output()
        .map_err(|error| format!("render proposed migration diff: {error}"))?;
    if !rendered.status.success() {
        return Err(format!(
            "render proposed migration diff: {}",
            String::from_utf8_lossy(&rendered.stderr).trim()
        ));
    }
    String::from_utf8(rendered.stdout)
        .map_err(|_| "proposed migration diff is not valid UTF-8".to_string())
}

fn collect_proposed_outputs(
    repo: &Path,
    changes: &[RepositoryTreeChange],
) -> Result<Vec<ProposedOutput>, String> {
    changes
        .iter()
        .filter(|change| change.action != RepositoryTreeAction::Unchanged)
        .map(|change| {
            let bytes =
                if change.action == RepositoryTreeAction::Delete {
                    None
                } else {
                    Some(std::fs::read(repo.join(&change.path)).map_err(|error| {
                        format!("read proposed output {}: {error}", change.path)
                    })?)
                };
            Ok(ProposedOutput {
                path: change.path.clone(),
                action: change.action,
                bytes,
                file_mode: change.file_mode.clone(),
            })
        })
        .collect()
}

fn acquire_upgrade_lock(repo: &Path) -> Result<UpgradeLock, String> {
    let common_dir = git_common_dir(repo)?;
    let state_dir = common_dir.join("aethyme-upgrades");
    ensure_private_directory(&state_dir)?;
    let lock_path = state_dir.join("upgrade.lock");
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let mut file = options
        .open(&lock_path)
        .map_err(|error| format!("open repository upgrade lock: {error}"))?;
    set_private_file_mode(&lock_path)?;
    #[cfg(unix)]
    {
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            return Err(
                "another repository upgrade or recovery holds the exclusive upgrade lock".into(),
            );
        }
    }
    #[cfg(not(unix))]
    {
        return Err("transactional repository upgrades require Unix file locking".into());
    }
    file.set_len(0)
        .map_err(|error| format!("initialize repository upgrade lock: {error}"))?;
    writeln!(file, "pid={}", std::process::id())
        .map_err(|error| format!("initialize repository upgrade lock: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("sync repository upgrade lock: {error}"))?;
    Ok(UpgradeLock { file })
}

fn git_common_dir(repo: &Path) -> Result<PathBuf, String> {
    let path = PathBuf::from(git(repo, &["rev-parse", "--git-common-dir"])?);
    Ok(if path.is_absolute() {
        path
    } else {
        repo.join(path)
    })
}

fn upgrade_state_dir(repo: &Path) -> Result<PathBuf, String> {
    let canonical = repo
        .canonicalize()
        .map_err(|error| format!("resolve repository root: {error}"))?;
    let worktree_key = sha256(&path_bytes(&canonical));
    let directory = git_common_dir(repo)?
        .join("aethyme-upgrades")
        .join(worktree_key);
    ensure_private_directory(&directory)?;
    Ok(directory)
}

fn journal_path(repo: &Path, plan_digest: &str) -> Result<PathBuf, String> {
    Ok(upgrade_state_dir(repo)?.join(format!("{plan_digest}.rollback.json")))
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_dir())
    {
        return Err(format!(
            "upgrade state path {} must be a directory",
            path.display()
        ));
    }
    std::fs::create_dir_all(path)
        .map_err(|error| format!("create upgrade state directory {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("protect upgrade state directory: {error}"))?;
    }
    Ok(())
}

fn set_private_file_mode(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("protect upgrade state file {}: {error}", path.display()))?;
    }
    Ok(())
}

fn validate_plan_digest(plan_digest: &str) -> Result<(), String> {
    if plan_digest.len() != 64 || !plan_digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("plan digest must be the full 64-character SHA-256".into());
    }
    Ok(())
}

fn sibling_transaction_path(
    relative: &str,
    plan_digest: &str,
    index: usize,
    suffix: &str,
) -> Result<String, String> {
    validate_relative_journal_path(relative)?;
    let path = Path::new(relative);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("upgrade path {relative} has no valid file name"))?;
    let name = format!(
        ".{file_name}.aethyme-upgrade-{}-{index}.{suffix}",
        &plan_digest[..12]
    );
    let sibling = path.parent().unwrap_or_else(|| Path::new("")).join(name);
    sibling
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| "repository upgrade paths must be valid UTF-8".to_string())
}

fn backup_path(path: &Path) -> Result<Option<UpgradeBackup>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "inspect rollback source {}: {error}",
                path.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "rollback source {} must be a regular non-symlink file",
            path.display()
        ));
    }
    let bytes = std::fs::read(path)
        .map_err(|error| format!("read rollback source {}: {error}", path.display()))?;
    Ok(Some(UpgradeBackup {
        sha256: sha256(&bytes),
        file_mode: regular_file_mode(&metadata),
        bytes,
    }))
}

fn build_rollback_journal(
    repo: &Path,
    plan: &RepositoryUpgradePlan,
    outputs: &[ProposedOutput],
) -> Result<UpgradeRollbackJournal, String> {
    let entries = outputs
        .iter()
        .enumerate()
        .map(|(index, output)| {
            let before = backup_path(&repo.join(&output.path))?;
            let planned_change = plan
                .changes
                .iter()
                .find(|change| change.path == output.path)
                .ok_or_else(|| format!("missing reviewed change {}", output.path))?;
            let after_sha256 = output.bytes.as_deref().map(sha256);
            if after_sha256 != planned_change.after_sha256
                || output.action != planned_change.action
                || output.file_mode != planned_change.file_mode
            {
                return Err(format!(
                    "reviewed output {} does not match the authorized plan",
                    output.path
                ));
            }
            let replacement_path = (output.action != RepositoryTreeAction::Delete)
                .then(|| {
                    sibling_transaction_path(&output.path, &plan.plan_digest, index, "replacement")
                })
                .transpose()?;
            let tombstone_path = (output.action == RepositoryTreeAction::Delete)
                .then(|| {
                    sibling_transaction_path(&output.path, &plan.plan_digest, index, "removed")
                })
                .transpose()?;
            for artifact in [&replacement_path, &tombstone_path].into_iter().flatten() {
                match std::fs::symlink_metadata(repo.join(artifact)) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Ok(_) => {
                        return Err(format!(
                            "transaction artifact path {artifact} already exists; preserve or remove it before creating a fresh plan"
                        ));
                    }
                    Err(error) => {
                        return Err(format!("inspect transaction artifact {artifact}: {error}"));
                    }
                }
            }
            Ok(UpgradeRollbackEntry {
                path: output.path.clone(),
                action: output.action,
                before,
                after_sha256,
                after_mode: output.file_mode.clone(),
                replacement_path,
                tombstone_path,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(UpgradeRollbackJournal {
        schema_version: TRANSACTION_SCHEMA_VERSION,
        plan_digest: plan.plan_digest.clone(),
        repository_head: plan.repository_head.clone(),
        existing_managed_state_digest: plan.existing_managed_state_digest.clone(),
        mode: plan.mode,
        marker_path: plan.mode.marker_path().into(),
        entries,
    })
}

fn write_rollback_journal(path: &Path, journal: &UpgradeRollbackJournal) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            return Err(format!(
                "rollback journal already exists for plan {}; run `aethyme upgrade recover --plan {}`",
                journal.plan_digest, journal.plan_digest
            ));
        }
        Ok(_) => {
            return Err(format!(
                "rollback journal path {} must be a regular non-symlink file",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("inspect rollback journal path: {error}")),
    }
    let mut bytes = serde_json::to_vec_pretty(journal).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    if bytes.len() > JOURNAL_MAX_BYTES {
        return Err(format!(
            "rollback journal would exceed the {} MiB safety limit; no repository files were replaced",
            JOURNAL_MAX_BYTES / 1024 / 1024
        ));
    }
    atomic_write_durable(path, &bytes, true)
}

fn prepare_replacement_files(
    repo: &Path,
    journal: &UpgradeRollbackJournal,
    outputs: &[ProposedOutput],
) -> Result<(), String> {
    for entry in &journal.entries {
        let Some(relative) = entry.replacement_path.as_deref() else {
            continue;
        };
        let output = outputs
            .iter()
            .find(|output| output.path == entry.path)
            .ok_or_else(|| format!("missing reviewed output {}", entry.path))?;
        let bytes = output
            .bytes
            .as_deref()
            .ok_or_else(|| format!("reviewed output {} has no content", entry.path))?;
        let path = repo.join(relative);
        write_new_file_durable(&path, bytes, entry.after_mode.as_deref())
            .map_err(|error| format!("prepare replacement for {}: {error}", entry.path))?;
    }
    Ok(())
}

fn apply_transaction_entry(repo: &Path, entry: &UpgradeRollbackEntry) -> Result<(), String> {
    let target = repo.join(&entry.path);
    match entry.action {
        RepositoryTreeAction::Create | RepositoryTreeAction::Update => {
            let replacement = repo.join(
                entry
                    .replacement_path
                    .as_deref()
                    .ok_or_else(|| format!("missing replacement path for {}", entry.path))?,
            );
            std::fs::rename(&replacement, &target)
                .map_err(|error| format!("atomically replace {}: {error}", entry.path))?;
            sync_parent(&target)?;
        }
        RepositoryTreeAction::Delete => {
            if target.exists() {
                let tombstone = repo.join(
                    entry
                        .tombstone_path
                        .as_deref()
                        .ok_or_else(|| format!("missing tombstone path for {}", entry.path))?,
                );
                if tombstone.exists() {
                    return Err(format!(
                        "rollback tombstone already exists for {}",
                        entry.path
                    ));
                }
                std::fs::rename(&target, &tombstone)
                    .map_err(|error| format!("atomically remove {}: {error}", entry.path))?;
                sync_parent(&target)?;
            }
        }
        RepositoryTreeAction::Unchanged => {}
    }
    Ok(())
}

fn verify_after_entry(repo: &Path, entry: &UpgradeRollbackEntry) -> Result<(), String> {
    let actual = backup_path(&repo.join(&entry.path))?;
    match entry.action {
        RepositoryTreeAction::Delete if actual.is_none() => Ok(()),
        RepositoryTreeAction::Delete => {
            Err(format!("verify deletion {}: path still exists", entry.path))
        }
        RepositoryTreeAction::Create | RepositoryTreeAction::Update => {
            let actual =
                actual.ok_or_else(|| format!("verify output {}: path is missing", entry.path))?;
            if Some(actual.sha256.as_str()) != entry.after_sha256.as_deref()
                || Some(actual.file_mode.as_str()) != entry.after_mode.as_deref()
            {
                return Err(format!(
                    "verify output {}: hash or mode differs from plan",
                    entry.path
                ));
            }
            Ok(())
        }
        RepositoryTreeAction::Unchanged => Ok(()),
    }
}

fn cleanup_transaction_artifacts(
    repo: &Path,
    journal: &UpgradeRollbackJournal,
) -> Result<(), String> {
    for entry in &journal.entries {
        for relative in [&entry.replacement_path, &entry.tombstone_path]
            .into_iter()
            .flatten()
        {
            let path = repo.join(relative);
            match std::fs::symlink_metadata(&path) {
                Ok(_) => remove_file_durable(&path)
                    .map_err(|error| format!("remove transaction artifact {relative}: {error}"))?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("inspect transaction artifact: {error}")),
            }
        }
    }
    Ok(())
}

#[cfg(debug_assertions)]
fn crash_if_requested(point: &str) {
    if std::env::var("AETHYME_TEST_UPGRADE_CRASH").as_deref() == Ok(point) {
        std::process::exit(CRASH_EXIT_CODE);
    }
}

#[cfg(not(debug_assertions))]
fn crash_if_requested(_point: &str) {}

fn execute_upgrade_transaction(
    repo: &Path,
    plan: &RepositoryUpgradePlan,
    outputs: &[ProposedOutput],
    customizations: &[RepositoryCustomization],
) -> Result<(), String> {
    let active_managed_state_digest = repository_state_digest(repo, plan.mode)?;
    let journal = build_rollback_journal(repo, plan, outputs)?;
    if repository_state_digest(repo, plan.mode)? != active_managed_state_digest {
        return Err(
            "managed repository state changed while the rollback journal was being prepared; no repository files were replaced"
                .into(),
        );
    }
    let journal_path = journal_path(repo, &plan.plan_digest)?;
    write_rollback_journal(&journal_path, &journal)?;
    crash_if_requested("after_journal");
    let result: Result<(), String> = (|| {
        prepare_replacement_files(repo, &journal, outputs)?;
        crash_if_requested("after_replacements_prepared");
        let mut replaced = 0;
        for entry in journal
            .entries
            .iter()
            .filter(|entry| entry.path != journal.marker_path)
        {
            verify_before_entry(repo, entry)?;
            apply_transaction_entry(repo, entry)?;
            replaced += 1;
            if replaced == 1 {
                crash_if_requested("after_first_replacement");
            }
        }
        crash_if_requested("after_replacements");
        for entry in journal
            .entries
            .iter()
            .filter(|entry| entry.path != journal.marker_path)
        {
            verify_after_entry(repo, entry)?;
        }
        let marker = journal
            .entries
            .iter()
            .find(|entry| entry.path == journal.marker_path)
            .ok_or_else(|| {
                "reviewed migration does not contain the repository marker".to_string()
            })?;
        verify_before_entry(repo, marker)?;
        apply_transaction_entry(repo, marker)?;
        crash_if_requested("after_marker");
        for entry in &journal.entries {
            verify_after_entry(repo, entry)?;
        }
        verify_deployment(repo, plan.mode, customizations)?;
        Ok(())
    })();
    if let Err(error) = result {
        return Err(format!(
            "{error}; rollback journal retained — run `aethyme upgrade recover --plan {}`",
            plan.plan_digest
        ));
    }
    cleanup_transaction_artifacts(repo, &journal).map_err(|error| {
        format!(
            "{error}; rollback journal retained — run `aethyme upgrade recover --plan {}`",
            plan.plan_digest
        )
    })?;
    remove_file_durable(&journal_path).map_err(|error| {
        format!(
            "{error}; verified migration completed but journal cleanup must be reconciled with `aethyme upgrade recover --plan {}`",
            plan.plan_digest
        )
    })?;
    Ok(())
}

fn read_rollback_journal(
    repo: &Path,
    plan_digest: &str,
) -> Result<(PathBuf, UpgradeRollbackJournal), String> {
    validate_plan_digest(plan_digest)?;
    let path = journal_path(repo, plan_digest)?;
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        format!(
            "no rollback journal for plan {plan_digest}: {error}; an in-progress marker alone never authorizes retry"
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("rollback journal must be a regular non-symlink file".into());
    }
    if metadata.len() > JOURNAL_MAX_BYTES as u64 {
        return Err("rollback journal exceeds the 64 MiB recovery limit".into());
    }
    let bytes = std::fs::read(&path).map_err(|error| format!("read rollback journal: {error}"))?;
    let journal: UpgradeRollbackJournal = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid rollback journal: {error}"))?;
    validate_rollback_journal(&journal, plan_digest)?;
    Ok((path, journal))
}

fn validate_rollback_journal(
    journal: &UpgradeRollbackJournal,
    plan_digest: &str,
) -> Result<(), String> {
    if journal.schema_version != TRANSACTION_SCHEMA_VERSION {
        return Err(format!(
            "unsupported rollback journal schema {}; expected {TRANSACTION_SCHEMA_VERSION}",
            journal.schema_version
        ));
    }
    if journal.plan_digest != plan_digest {
        return Err("rollback journal plan digest does not match --plan".into());
    }
    validate_sha256(
        &journal.existing_managed_state_digest,
        "journaled managed state",
    )?;
    if journal.marker_path != journal.mode.marker_path() {
        return Err("rollback journal marker path does not match its repository mode".into());
    }
    let mut paths = BTreeSet::new();
    if journal.entries.is_empty() {
        return Err("rollback journal contains no repository changes".into());
    }
    for (index, entry) in journal.entries.iter().enumerate() {
        validate_relative_journal_path(&entry.path)?;
        if !paths.insert(entry.path.clone()) {
            return Err(format!("rollback journal repeats path {}", entry.path));
        }
        if let Some(before) = &entry.before
            && sha256(&before.bytes) != before.sha256
        {
            return Err(format!("rollback backup hash mismatch for {}", entry.path));
        }
        if let Some(before) = &entry.before {
            validate_sha256(&before.sha256, "rollback backup")?;
            validate_file_mode(&before.file_mode)?;
        }
        if let Some(after) = &entry.after_sha256 {
            validate_sha256(after, "reviewed output")?;
        }
        if let Some(mode) = &entry.after_mode {
            validate_file_mode(mode)?;
        }
        let expected_replacement = matches!(
            entry.action,
            RepositoryTreeAction::Create | RepositoryTreeAction::Update
        )
        .then(|| sibling_transaction_path(&entry.path, &journal.plan_digest, index, "replacement"))
        .transpose()?;
        let expected_tombstone = (entry.action == RepositoryTreeAction::Delete)
            .then(|| sibling_transaction_path(&entry.path, &journal.plan_digest, index, "removed"))
            .transpose()?;
        if entry.replacement_path != expected_replacement
            || entry.tombstone_path != expected_tombstone
        {
            return Err(format!(
                "rollback journal transaction paths are invalid for {}",
                entry.path
            ));
        }
        match entry.action {
            RepositoryTreeAction::Create | RepositoryTreeAction::Update
                if entry.after_sha256.is_some() && entry.after_mode.is_some() => {}
            RepositoryTreeAction::Delete if entry.after_sha256.is_none() => {}
            RepositoryTreeAction::Unchanged => {
                return Err(format!(
                    "rollback journal contains unchanged path {}",
                    entry.path
                ));
            }
            _ => {
                return Err(format!(
                    "rollback journal action and output disagree for {}",
                    entry.path
                ));
            }
        }
    }
    let marker = journal
        .entries
        .iter()
        .find(|entry| entry.path == journal.marker_path)
        .ok_or("rollback journal does not contain the repository marker")?;
    if !matches!(
        marker.action,
        RepositoryTreeAction::Create | RepositoryTreeAction::Update
    ) {
        return Err("rollback journal does not install the repository marker last".into());
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} contains an invalid SHA-256"));
    }
    Ok(())
}

fn validate_file_mode(mode: &str) -> Result<(), String> {
    if matches!(mode, "100644" | "100755") {
        Ok(())
    } else {
        Err(format!("rollback journal contains unsupported mode {mode}"))
    }
}

fn verify_recovery_state_is_known(
    repo: &Path,
    journal: &UpgradeRollbackJournal,
) -> Result<(), String> {
    for entry in &journal.entries {
        let actual = backup_path(&repo.join(&entry.path))?;
        let known_target = match actual.as_ref() {
            None => entry.before.is_none() || entry.action == RepositoryTreeAction::Delete,
            Some(actual) => {
                entry.before.as_ref().is_some_and(|before| {
                    before.sha256 == actual.sha256 && before.file_mode == actual.file_mode
                }) || (entry.after_sha256.as_deref() == Some(actual.sha256.as_str())
                    && entry.after_mode.as_deref() == Some(actual.file_mode.as_str()))
            }
        };
        if !known_target {
            return Err(format!(
                "recovery refused: {} changed after the interrupted upgrade; preserve that work and resolve it explicitly",
                entry.path
            ));
        }
        if let Some(relative) = &entry.replacement_path
            && let Some(replacement) = backup_path(&repo.join(relative))?
            && (entry.after_sha256.as_deref() != Some(replacement.sha256.as_str())
                || entry.after_mode.as_deref() != Some(replacement.file_mode.as_str()))
        {
            return Err(format!(
                "recovery replacement hash mismatch for {}",
                entry.path
            ));
        }
        if let Some(relative) = &entry.tombstone_path
            && let Some(tombstone) = backup_path(&repo.join(relative))?
            && !entry.before.as_ref().is_some_and(|before| {
                before.sha256 == tombstone.sha256 && before.file_mode == tombstone.file_mode
            })
        {
            return Err(format!(
                "recovery tombstone hash mismatch for {}",
                entry.path
            ));
        }
    }
    Ok(())
}

fn restore_transaction_entry(
    repo: &Path,
    journal: &UpgradeRollbackJournal,
    entry: &UpgradeRollbackEntry,
    index: usize,
) -> Result<(), String> {
    let target = repo.join(&entry.path);
    if let Some(before) = &entry.before {
        let recovery_relative =
            sibling_transaction_path(&entry.path, &journal.plan_digest, index, "recovery")?;
        let recovery = repo.join(&recovery_relative);
        match backup_path(&recovery)? {
            None => write_new_file_durable(&recovery, &before.bytes, Some(&before.file_mode))?,
            Some(existing)
                if existing.sha256 == before.sha256 && existing.file_mode == before.file_mode => {}
            Some(_) => {
                return Err(format!(
                    "recovery temporary file for {} is not the journaled before-state",
                    entry.path
                ));
            }
        }
        std::fs::rename(&recovery, &target)
            .map_err(|error| format!("restore {}: {error}", entry.path))?;
        sync_parent(&target)?;
    } else if target.exists() {
        let removed_relative = entry
            .replacement_path
            .clone()
            .unwrap_or(sibling_transaction_path(
                &entry.path,
                &journal.plan_digest,
                index,
                "recovery-removed",
            )?);
        let removed = repo.join(&removed_relative);
        if removed.exists() {
            return Err(format!(
                "recovery cannot remove {} while its transaction temporary file also exists",
                entry.path
            ));
        }
        std::fs::rename(&target, &removed)
            .map_err(|error| format!("remove created output {}: {error}", entry.path))?;
        sync_parent(&target)?;
        remove_file_durable(&removed)?;
    }
    Ok(())
}

fn verify_before_entry(repo: &Path, entry: &UpgradeRollbackEntry) -> Result<(), String> {
    let actual = backup_path(&repo.join(&entry.path))?;
    match (&entry.before, actual) {
        (None, None) => Ok(()),
        (Some(before), Some(actual))
            if before.sha256 == actual.sha256 && before.file_mode == actual.file_mode =>
        {
            Ok(())
        }
        _ => Err(format!("recovery verification failed for {}", entry.path)),
    }
}

fn recover(repo_hint: &Path, plan_digest: &str) -> Result<RepositoryUpgradeRecovery, String> {
    validate_plan_digest(plan_digest)?;
    let repo = git_root(repo_hint)?;
    let _lock = acquire_upgrade_lock(&repo)?;
    let (path, journal) = read_rollback_journal(&repo, plan_digest)?;
    let current_head = git(&repo, &["rev-parse", "HEAD"])?;
    if current_head != journal.repository_head {
        return Err(format!(
            "recovery refused: repository HEAD moved from {} to {current_head} after the interrupted upgrade",
            journal.repository_head
        ));
    }
    verify_recovery_state_is_known(&repo, &journal)?;
    let mut ordered = journal
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.path != journal.marker_path)
        .collect::<Vec<_>>();
    ordered.extend(
        journal
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.path == journal.marker_path),
    );
    let mut restored = 0;
    for (index, entry) in ordered {
        restore_transaction_entry(&repo, &journal, entry, index)?;
        restored += 1;
        if restored == 1 {
            crash_if_requested("after_first_recovery_restore");
        }
    }
    crash_if_requested("after_recovery_restores");
    for entry in &journal.entries {
        verify_before_entry(&repo, entry)?;
    }
    cleanup_transaction_artifacts(&repo, &journal)?;
    crash_if_requested("after_recovery_cleanup");
    remove_file_durable(&path)?;
    let restored_paths = journal
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    Ok(RepositoryUpgradeRecovery {
        schema_version: TRANSACTION_SCHEMA_VERSION,
        plan_digest: plan_digest.into(),
        recovered: true,
        restored_paths,
        next_action: "review the restored repository, then create and confirm a fresh upgrade plan"
            .into(),
    })
}

#[cfg(unix)]
fn set_file_mode(path: &Path, mode: Option<&str>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = match mode {
        Some("100755") => 0o755,
        Some("100644") | None => 0o644,
        Some(other) => return Err(format!("unsupported reviewed file mode {other}")),
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(permissions))
        .map_err(|error| format!("set reviewed output mode {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path, _mode: Option<&str>) -> Result<(), String> {
    Ok(())
}

fn active_session_preconditions(
    repo: &Path,
    excluded_session_id: Option<i64>,
) -> Result<Vec<UpgradeActiveSessionPrecondition>, String> {
    let checkout = aethyme_broker::GitRepo::discover(repo).map_err(|error| error.to_string())?;
    let main_root = checkout.main_root().map_err(|error| error.to_string())?;
    if !main_root.join(aethyme_broker::BROKER_DB_RELPATH).is_file() {
        return Ok(Vec::new());
    }
    let mut broker =
        aethyme_broker::Broker::open_snapshot(repo).map_err(|error| error.to_string())?;
    let mut sessions = broker
        .store()
        .live_sessions()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|session| Some(session.id) != excluded_session_id)
        .filter(|session| {
            matches!(
                session.status,
                aethyme_broker::SessionStatus::Active
                    | aethyme_broker::SessionStatus::Idle
                    | aethyme_broker::SessionStatus::Stale
            )
        })
        .map(|session| {
            let contract = session.repository_contract;
            UpgradeActiveSessionPrecondition {
                session_id: session.id,
                status: session.status,
                repository_schema: contract
                    .as_ref()
                    .and_then(|contract| contract.repository_schema),
                deployment_state_digest: contract
                    .as_ref()
                    .map(|contract| contract.deployment_state_digest.clone()),
                aethyme_version: contract
                    .as_ref()
                    .map(|contract| contract.aethyme_version.clone()),
                gate_definition_digest: contract
                    .and_then(|contract| contract.gate_definition_digest),
            }
        })
        .collect::<Vec<_>>();
    sessions.sort_by_key(|session| session.session_id);
    Ok(sessions)
}

fn dirty_paths(repo: &Path) -> Result<Vec<String>, String> {
    let output = git_bytes(
        repo,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    let records = output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut paths = BTreeSet::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if record.len() < 4 || record[2] != b' ' {
            return Err("cannot parse repository dirty-path status".into());
        }
        let status = &record[..2];
        let path = std::str::from_utf8(&record[3..])
            .map_err(|_| "repository upgrade paths must be valid UTF-8".to_string())?;
        paths.insert(path.to_string());
        if status.iter().any(|byte| matches!(byte, b'R' | b'C')) {
            index += 1;
            let source = records
                .get(index)
                .ok_or_else(|| "cannot parse renamed repository dirty path".to_string())?;
            let source = std::str::from_utf8(source)
                .map_err(|_| "repository upgrade paths must be valid UTF-8".to_string())?;
            paths.insert(source.to_string());
        }
        index += 1;
    }
    Ok(paths.into_iter().collect())
}

fn paths_overlap(left: &str, right: &str) -> bool {
    left == right
        || (left.ends_with('/') && right.starts_with(left))
        || (right.ends_with('/') && left.starts_with(right))
}

fn relevant_leases(
    repo: &Path,
    planned_paths: &[String],
    excluded_session_id: Option<i64>,
) -> Result<Vec<UpgradeRelevantLease>, String> {
    let checkout = aethyme_broker::GitRepo::discover(repo).map_err(|error| error.to_string())?;
    let main_root = checkout.main_root().map_err(|error| error.to_string())?;
    if !main_root.join(aethyme_broker::BROKER_DB_RELPATH).is_file() {
        return Ok(Vec::new());
    }
    let mut broker =
        aethyme_broker::Broker::open_snapshot(repo).map_err(|error| error.to_string())?;
    let mut leases = broker
        .store()
        .active_leases()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|lease| Some(lease.session_id) != excluded_session_id)
        .filter_map(|lease| {
            let overlapping_planned_paths = planned_paths
                .iter()
                .filter(|path| paths_overlap(&lease.path, path))
                .cloned()
                .collect::<Vec<_>>();
            (!overlapping_planned_paths.is_empty()).then_some(UpgradeRelevantLease {
                lease_id: lease.id,
                session_id: lease.session_id,
                path: lease.path,
                kind: lease.kind,
                expires_at: lease.expires_at,
                overlapping_planned_paths,
            })
        })
        .collect::<Vec<_>>();
    leases.sort_by(|left, right| {
        left.session_id
            .cmp(&right.session_id)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.lease_id.cmp(&right.lease_id))
    });
    Ok(leases)
}

fn is_shared_policy_or_gate_change(change: &RepositoryTreeChange) -> bool {
    change.action != RepositoryTreeAction::Unchanged
        && change.ownership != RepositoryPathOwnership::AethymeOwned
}

fn build_plan(
    repo_hint: &Path,
    requested_mode: Option<RepositoryMode>,
    resolution_file: Option<&Path>,
    excluded_session_id: Option<i64>,
) -> Result<BuiltUpgradePlan, String> {
    let repo = git_root(repo_hint)?;
    let requested_resolutions = load_resolution_file(&repo, resolution_file)?;
    let repository_head = git(&repo, &["rev-parse", "HEAD"])?;
    let proposal = ProposedRepository::from_committed_head(&repo, &repository_head)?;
    let proposed_repo = proposal.root();
    let detected = detect_mode(proposed_repo).or_else(|| {
        // Local-only enrollment is deliberately clone-local and therefore
        // absent from committed HEAD. Consult only its exact control markers;
        // never let dirty or untracked repository content select a migration.
        detect_local_only_enrollment(&repo).then_some(RepositoryMode::LocalOnly)
    });
    let mode = requested_mode.or(detected).ok_or_else(|| {
        "repository is not enrolled; use `aethyme deploy` (or `aethyme deploy --local-only`) for first-time setup".to_string()
    })?;
    if let (Some(detected), Some(requested)) = (detected, requested_mode)
        && detected != requested
    {
        return Err(format!(
            "repository is enrolled as {detected:?}, not {requested:?}"
        ));
    }

    let mut blockers = Vec::new();
    let marker = match read_marker(proposed_repo, mode) {
        Ok(marker) => marker,
        Err(error) => {
            blockers.push(error);
            None
        }
    };
    let from_schema = match marker.as_ref() {
        Some(marker) => marker.schema_version,
        None => 0,
    };
    if let Some(marker) = marker.as_ref()
        && marker.schema_version == REPOSITORY_SCHEMA_VERSION
        && !marker_is_current(marker)
    {
        blockers.push(format!(
            "{} is incomplete: required migration {MIGRATION_ID} is not recorded",
            mode.marker_path()
        ));
    }
    if from_schema > REPOSITORY_SCHEMA_VERSION {
        blockers.push(format!(
            "repository schema {from_schema} is newer than supported schema {REPOSITORY_SCHEMA_VERSION}"
        ));
    }
    let managed_paths = managed_paths(mode);
    let mut resolution_paths = BTreeSet::new();
    for relative in &managed_paths {
        if let Some(blocker) = managed_path_blocker(proposed_repo, relative)? {
            resolution_paths.insert(relative.clone());
            blockers.push(blocker);
        }
    }
    let (customizations, mut resolution_choices, customization_blockers) = assess_customizations(
        proposed_repo,
        mode,
        &requested_resolutions,
        &resolution_paths,
    )?;
    blockers.extend(customization_blockers);
    let migrations = EMBEDDED_MIGRATIONS
        .iter()
        .filter(|migration| {
            migration.from_schema >= from_schema && migration.to_schema <= REPOSITORY_SCHEMA_VERSION
        })
        .map(|migration| migration.id.to_string())
        .collect::<Vec<_>>();
    let mut examined_paths = committed_paths(proposed_repo, &repository_head)?;
    examined_paths.extend(managed_paths.iter().cloned());
    examined_paths.sort();
    examined_paths.dedup();
    let existing_managed_state_digest = repository_state_digest(proposed_repo, mode)?;
    let compatibility = compatibility_decision(
        &repo,
        CommandCapability::Upgrade,
        CompatibilityContext {
            session_contract: None,
            surface: InvocationSurface::UpgradeCommand,
        },
    )
    .or_else(|| {
        (detected.is_none() && requested_mode == Some(RepositoryMode::Canonical)).then(|| {
            CompatibilityDecision {
                repository: RepositoryCompatibility::UpgradeRequired,
                repository_schema: Some(0),
                capability: CommandCapability::Upgrade,
                surface: InvocationSurface::UpgradeCommand,
                allowed: true,
                severity: CompatibilitySeverity::Info,
                execution: CompatibilityExecution::Normal,
                reason: "repository is ready for reviewed first enrollment".into(),
                remediation: None,
            }
        })
    })
    .ok_or_else(|| "cannot determine repository upgrade compatibility".to_string())?;
    let active_sessions = active_session_preconditions(&repo, excluded_session_id)?;
    let before_tree = snapshot_paths(proposed_repo, &examined_paths)?;
    if blockers.is_empty() && !migrations.is_empty() {
        write_pending_marker(proposed_repo, mode)?;
        match mode {
            RepositoryMode::Canonical => migrate_canonical(proposed_repo, &customizations)?,
            RepositoryMode::LocalOnly => migrate_local(proposed_repo, &customizations)?,
        }
        write_current_marker(proposed_repo, mode)?;
        verify_deployment(proposed_repo, mode, &customizations)?;
    }
    let mut change_paths = managed_paths;
    change_paths.extend(proposed_changed_paths(proposed_repo)?);
    change_paths.sort();
    change_paths.dedup();
    examined_paths.extend(change_paths.iter().cloned());
    examined_paths.sort();
    examined_paths.dedup();
    let after_tree = snapshot_paths(proposed_repo, &change_paths)?;
    let changes = classify_tree_changes(
        mode,
        &change_paths,
        &before_tree,
        &after_tree,
        &resolution_paths,
    );
    let planned_paths = changes
        .iter()
        .filter(|change| change.action != RepositoryTreeAction::Unchanged)
        .map(|change| change.path.clone())
        .collect::<Vec<_>>();
    for change in changes.iter().filter(|change| change.requires_resolution) {
        if !resolution_choices
            .iter()
            .any(|resolution| resolution.path == change.path)
        {
            resolution_choices.push(RepositoryResolution {
                path: change.path.clone(),
                choice: RepositoryResolutionChoice::Unresolved,
            });
        }
    }
    resolution_choices.sort_by(|left, right| left.path.cmp(&right.path));
    let migration_diff = render_migration_diff(proposed_repo, &planned_paths)?;
    let proposed_outputs = collect_proposed_outputs(proposed_repo, &changes)?;
    let diff_sha256 = sha256(migration_diff.as_bytes());
    let dirty_paths = dirty_paths(&repo)?;
    let (overlapping_dirty_paths, disjoint_dirty_paths): (Vec<_>, Vec<_>) =
        dirty_paths.iter().cloned().partition(|dirty| {
            planned_paths
                .iter()
                .any(|planned| paths_overlap(dirty, planned))
        });
    let relevant_leases = relevant_leases(&repo, &planned_paths, excluded_session_id)?;
    let shared_policy_or_gate_migration = changes.iter().any(is_shared_policy_or_gate_change);
    let mut warnings = Vec::new();
    if !disjoint_dirty_paths.is_empty() {
        warnings.push(
            "uncommitted disjoint changes were not inputs to the proposed repository tree; apply will touch only the exact reviewed planned paths"
                .into(),
        );
    }
    if !overlapping_dirty_paths.is_empty() {
        blockers.push(format!(
            "uncommitted changes overlap proposed repository writes: {}; commit them through the managed Aethyme pre-commit lane before upgrading",
            overlapping_dirty_paths.join(", ")
        ));
    }
    if shared_policy_or_gate_migration && !active_sessions.is_empty() {
        blockers.push(format!(
            "shared policy or gate migration is blocked while broker sessions are active: {}; finish each session with `aethyme broker finish --session <id>` before upgrading",
            active_sessions
                .iter()
                .map(|session| session.session_id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let safe = blockers.is_empty();
    let digest_input = PlanDigest {
        schema_version: PLAN_SCHEMA_VERSION,
        mode,
        from_schema,
        to_schema: REPOSITORY_SCHEMA_VERSION,
        repository_head: &repository_head,
        existing_managed_state_digest: &existing_managed_state_digest,
        compatibility: &compatibility,
        active_sessions: &active_sessions,
        dirty_paths: &dirty_paths,
        overlapping_dirty_paths: &overlapping_dirty_paths,
        disjoint_dirty_paths: &disjoint_dirty_paths,
        relevant_leases: &relevant_leases,
        shared_policy_or_gate_migration,
        warnings: &warnings,
        migrations: &migrations,
        changes: &changes,
        customizations: &customizations,
        resolution_choices: &resolution_choices,
        planned_paths: &planned_paths,
        examined_paths: &examined_paths,
        diff_sha256: &diff_sha256,
        safe,
        blockers: &blockers,
    };
    let plan_digest =
        sha256(&serde_json::to_vec(&digest_input).map_err(|error| error.to_string())?);
    let resolution_argument = resolution_file
        .map(|path| format!(" --resolution-file {}", path.display()))
        .unwrap_or_default();
    let next_action = if !overlapping_dirty_paths.is_empty() {
        "commit the overlapping paths through the managed Aethyme pre-commit lane, then regenerate the upgrade plan"
            .into()
    } else if shared_policy_or_gate_migration && !active_sessions.is_empty() {
        "finish every listed broker session, then regenerate the upgrade plan; disjoint worktree changes do not need to be moved"
            .into()
    } else if !safe {
        "create a schema-1 resolution file choosing preserve, merge, or replace for every customized policy, then regenerate the upgrade plan with --resolution-file <path>"
            .into()
    } else if migrations.is_empty() {
        "repository deployment is current; no migration is required".into()
    } else {
        format!(
            "review this plan, then run `aethyme upgrade apply --repo .{resolution_argument} --confirm {plan_digest}`"
        )
    };
    Ok(BuiltUpgradePlan {
        report: RepositoryUpgradePlan {
            schema_version: PLAN_SCHEMA_VERSION,
            mode,
            from_schema,
            to_schema: REPOSITORY_SCHEMA_VERSION,
            repository_head,
            existing_managed_state_digest,
            compatibility,
            active_sessions,
            dirty_paths,
            overlapping_dirty_paths,
            disjoint_dirty_paths,
            relevant_leases,
            shared_policy_or_gate_migration,
            warnings,
            migrations,
            changes,
            customizations,
            resolution_choices,
            planned_paths,
            examined_paths,
            diff_sha256,
            applied: false,
            safe,
            blockers,
            plan_digest,
            next_action,
        },
        migration_diff,
        proposed_outputs,
    })
}

pub fn plan(
    repo_hint: &Path,
    requested_mode: Option<RepositoryMode>,
) -> Result<RepositoryUpgradePlan, String> {
    build_plan(repo_hint, requested_mode, None, None).map(|built| built.report)
}

/// Build the exact tracked tree for first canonical enrollment. The optional
/// session is the isolated enrollment worker itself; excluding it prevents
/// that worker from making its own reviewed migration appear unsafe.
pub fn initial_enrollment_plan(
    repo_hint: &Path,
    enrollment_session_id: Option<i64>,
) -> Result<RepositoryUpgradePlan, String> {
    build_plan(
        repo_hint,
        Some(RepositoryMode::Canonical),
        None,
        enrollment_session_id,
    )
    .map(|built| built.report)
}

pub fn apply(
    repo_hint: &Path,
    requested_mode: Option<RepositoryMode>,
    confirmation: &str,
) -> Result<RepositoryUpgradePlan, String> {
    apply_with_resolution_file(repo_hint, requested_mode, confirmation, None)
}

pub fn apply_initial_enrollment(
    repo_hint: &Path,
    enrollment_session_id: i64,
    confirmation: &str,
) -> Result<RepositoryUpgradePlan, String> {
    apply_with_resolution_file_excluding_session(
        repo_hint,
        Some(RepositoryMode::Canonical),
        confirmation,
        None,
        Some(enrollment_session_id),
    )
}

fn refuse_unjournaled_in_progress_marker(
    repo: &Path,
    requested_mode: Option<RepositoryMode>,
) -> Result<(), String> {
    let Some(mode) = requested_mode.or_else(|| detect_mode(repo)) else {
        return Ok(());
    };
    if read_marker(repo, mode)?
        .as_ref()
        .is_some_and(marker_upgrade_is_in_progress)
    {
        return Err(format!(
            "{} records an interrupted legacy upgrade; an in-progress marker never authorizes retry, and no rollback journal can be inferred — restore the repository from reviewed Git state before creating a new plan",
            mode.marker_path()
        ));
    }
    Ok(())
}

fn apply_with_resolution_file(
    repo_hint: &Path,
    requested_mode: Option<RepositoryMode>,
    confirmation: &str,
    resolution_file: Option<&Path>,
) -> Result<RepositoryUpgradePlan, String> {
    apply_with_resolution_file_excluding_session(
        repo_hint,
        requested_mode,
        confirmation,
        resolution_file,
        None,
    )
}

fn apply_with_resolution_file_excluding_session(
    repo_hint: &Path,
    requested_mode: Option<RepositoryMode>,
    confirmation: &str,
    resolution_file: Option<&Path>,
    excluded_session_id: Option<i64>,
) -> Result<RepositoryUpgradePlan, String> {
    validate_plan_digest(confirmation)
        .map_err(|_| "--confirm must be the full 64-character plan SHA-256".to_string())?;
    let repo = git_root(repo_hint)?;
    let _lock = acquire_upgrade_lock(&repo)?;
    refuse_unjournaled_in_progress_marker(&repo, requested_mode)?;
    let built = build_plan(
        repo_hint,
        requested_mode,
        resolution_file,
        excluded_session_id,
    )?;
    let proposed_outputs = built.proposed_outputs;
    let mut before = built.report;
    if !before.safe {
        return Err(format!(
            "repository upgrade is blocked: {}",
            before.blockers.join("; ")
        ));
    }
    if before.plan_digest != confirmation {
        return Err(format!(
            "repository changed after review; expected confirmation {}, received {confirmation}; regenerate the plan",
            before.plan_digest
        ));
    }
    if before.migrations.is_empty() {
        return Ok(before);
    }

    ensure_apply_worktree_safe(&repo, &before, excluded_session_id)?;
    execute_upgrade_transaction(&repo, &before, &proposed_outputs, &before.customizations)?;
    before.applied = true;
    before.next_action = match before.mode {
        RepositoryMode::Canonical => {
            "review and commit the migrated repository files; retain `aethyme deploy verify` in CI"
                .into()
        }
        RepositoryMode::LocalOnly => {
            "local-only repository migration complete; other clones remain unchanged".into()
        }
    };
    Ok(before)
}

fn migrate_canonical(
    repo: &Path,
    customizations: &[RepositoryCustomization],
) -> Result<(), String> {
    aethyme_broker::init::scaffold(repo).map_err(|error| error.to_string())?;
    apply_customization_migrations(repo, RepositoryMode::Canonical, customizations)
}

fn migrate_local(repo: &Path, customizations: &[RepositoryCustomization]) -> Result<(), String> {
    aethyme_broker::init::scaffold_local(repo).map_err(|error| error.to_string())?;
    apply_customization_migrations(repo, RepositoryMode::LocalOnly, customizations)
}

fn verify_managed_policy(repo: &Path, relative: &str) -> Result<(), String> {
    let actual = std::fs::read_to_string(repo.join(relative))
        .map_err(|error| format!("post-migration verification failed for {relative}: {error}"))?;
    let canonical = aethyme_enhance::agents::render_agents_document(Some(repo))?;
    let expected = marked_policy(&canonical);
    let begin_count = actual.matches(aethyme_enhance::BLOCK_BEGIN).count();
    let end_count = actual.matches(aethyme_enhance::BLOCK_END).count();
    if begin_count != 1
        || end_count != 1
        || aethyme_enhance::render::splice_generated_block(&actual, &expected) != actual
    {
        return Err(format!(
            "post-migration verification failed: {relative} does not contain exactly one current Aethyme managed block"
        ));
    }
    Ok(())
}

fn verify_deployment(
    repo: &Path,
    mode: RepositoryMode,
    customizations: &[RepositoryCustomization],
) -> Result<(), String> {
    verify_current_marker(repo, mode)?;
    match mode {
        RepositoryMode::Canonical => {
            let results = aethyme_enhance::deploy::verify(repo)?;
            let failures = results
                .iter()
                .filter(|result| !result.exists || result.placeholder_present)
                .filter(|result| {
                    !matches!(result.relative_path.as_str(), "AGENTS.md" | "CLAUDE.md")
                })
                .map(|result| result.relative_path.clone())
                .collect::<Vec<_>>();
            if !failures.is_empty() {
                return Err(format!(
                    "post-migration verification failed: {}",
                    failures.join(", ")
                ));
            }
            for relative in ["AGENTS.md", "CLAUDE.md"] {
                let preserved = resolved_choice(customizations, relative)
                    == Some(RepositoryResolutionChoice::Preserve);
                if !preserved {
                    verify_managed_policy(repo, relative)?;
                }
            }
        }
        RepositoryMode::LocalOnly => {
            let preserved =
                resolved_choice(customizations, aethyme_enhance::local::LOCAL_POLICY_PATH)
                    == Some(RepositoryResolutionChoice::Preserve);
            let failures = aethyme_enhance::local::verify(repo)?
                .into_iter()
                .filter(|failure| {
                    !(preserved && failure.contains(aethyme_enhance::local::LOCAL_POLICY_PATH))
                })
                .collect::<Vec<_>>();
            if !failures.is_empty() {
                return Err(format!(
                    "post-migration verification failed: {}",
                    failures.join("; ")
                ));
            }
            if !preserved {
                verify_managed_policy(repo, aethyme_enhance::local::LOCAL_POLICY_PATH)?;
            }
        }
    }
    Ok(())
}

fn detect_mode(repo: &Path) -> Option<RepositoryMode> {
    if detect_local_only_enrollment(repo) {
        Some(RepositoryMode::LocalOnly)
    } else if repo.join(CANONICAL_MARKER_PATH).is_file()
        || repo.join(".codex/skills/aethyme/SKILL.md").is_file()
        || std::fs::read_to_string(repo.join("AGENTS.md"))
            .map(|text| text.contains("Broker Coordination"))
            .unwrap_or(false)
    {
        Some(RepositoryMode::Canonical)
    } else {
        None
    }
}

fn detect_local_only_enrollment(repo: &Path) -> bool {
    repo.join(aethyme_enhance::local::LOCAL_MARKER_PATH)
        .is_file()
        || repo.join(LOCAL_MARKER_PATH).is_file()
}

fn managed_paths(mode: RepositoryMode) -> Vec<String> {
    aethyme_broker::repository_managed_paths(match mode {
        RepositoryMode::Canonical => aethyme_broker::RepositoryDeploymentMode::Canonical,
        RepositoryMode::LocalOnly => aethyme_broker::RepositoryDeploymentMode::LocalOnly,
    })
}

fn managed_path_blocker(repo: &Path, relative: &str) -> Result<Option<String>, String> {
    let components = Path::new(relative).components().collect::<Vec<_>>();
    let mut current = repo.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        current.push(component.as_os_str());
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("inspect {relative}: {error}")),
        };
        if metadata.file_type().is_symlink() {
            return Ok(Some(format!(
                "managed path {relative} crosses symlink {}; repository upgrades never write through symlinks",
                current.strip_prefix(repo).unwrap_or(&current).display()
            )));
        }
        let final_component = index + 1 == components.len();
        if final_component && !metadata.is_file() {
            return Ok(Some(format!(
                "managed path {relative} exists but is not a regular file"
            )));
        }
        if !final_component && !metadata.is_dir() {
            return Ok(Some(format!(
                "managed path {relative} crosses non-directory {}",
                current.strip_prefix(repo).unwrap_or(&current).display()
            )));
        }
    }
    Ok(None)
}

fn ensure_apply_worktree_safe(
    repo: &Path,
    plan: &RepositoryUpgradePlan,
    excluded_session_id: Option<i64>,
) -> Result<(), String> {
    let current_head = git(repo, &["rev-parse", "HEAD"])?;
    if current_head != plan.repository_head {
        return Err(format!(
            "repository HEAD moved after review; planned {}, found {current_head}; regenerate the plan",
            plan.repository_head
        ));
    }
    let current_dirty_paths = dirty_paths(repo)?;
    if current_dirty_paths != plan.dirty_paths {
        return Err(
            "repository dirty paths changed after review; regenerate the upgrade plan before applying exact reviewed outputs"
                .into(),
        );
    }
    if !plan.overlapping_dirty_paths.is_empty() {
        return Err(format!(
            "uncommitted changes overlap proposed repository writes: {}; commit them through the managed Aethyme pre-commit lane before upgrading",
            plan.overlapping_dirty_paths.join(", ")
        ));
    }
    let current_sessions = active_session_preconditions(repo, excluded_session_id)?;
    if current_sessions != plan.active_sessions {
        return Err(
            "live broker sessions changed after review; regenerate the upgrade plan before applying"
                .into(),
        );
    }
    if plan.shared_policy_or_gate_migration && !current_sessions.is_empty() {
        return Err(
            "shared policy or gate migration is blocked while broker sessions are active; finish each listed session before upgrading"
                .into(),
        );
    }
    let current_leases = relevant_leases(repo, &plan.planned_paths, excluded_session_id)?;
    if current_leases != plan.relevant_leases {
        return Err(
            "relevant broker leases changed after review; regenerate the upgrade plan before applying"
                .into(),
        );
    }
    for relative in &plan.planned_paths {
        if let Some(blocker) = managed_path_blocker(repo, relative)? {
            return Err(blocker);
        }
    }
    Ok(())
}

fn repository_state_digest(repo: &Path, mode: RepositoryMode) -> Result<String, String> {
    aethyme_broker::repository_state_digest(
        repo,
        match mode {
            RepositoryMode::Canonical => aethyme_broker::RepositoryDeploymentMode::Canonical,
            RepositoryMode::LocalOnly => aethyme_broker::RepositoryDeploymentMode::LocalOnly,
        },
    )
}

fn git_root(repo_hint: &Path) -> Result<PathBuf, String> {
    let root = git(repo_hint, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(root))
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_bytes(repo, args)?;
    Ok(String::from_utf8_lossy(&output).trim().to_string())
}

fn git_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|error| format!("run git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_relative_journal_path(relative: &str) -> Result<(), String> {
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "invalid repository-relative upgrade path {relative:?}"
        ));
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync directory {}: {error}", parent.display()))
}

fn atomic_write_durable(path: &Path, bytes: &[u8], private: bool) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    let mut file = options
        .open(&temporary)
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    if private {
        set_private_file_mode(&temporary)?;
    }
    let result = (|| {
        file.write_all(bytes)
            .map_err(|error| format!("{}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("{}: {error}", temporary.display()))?;
        std::fs::rename(&temporary, path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn write_new_file_durable(
    path: &Path,
    bytes: &[u8],
    file_mode: Option<&str>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    set_file_mode(path, file_mode)?;
    file.sync_all()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(())
}

fn remove_file_durable(path: &Path) -> Result<(), String> {
    std::fs::remove_file(path).map_err(|error| format!("{}: {error}", path.display()))?;
    sync_parent(path)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temporary, bytes)
        .map_err(|error| format!("{}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("{}: {error}", path.display())
    })
}

pub fn run(args: &[String]) -> u8 {
    match run_inner(args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("aethyme upgrade: {error}");
            1
        }
    }
}

fn run_inner(args: &[String]) -> Result<(), String> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        print_usage();
        return Ok(());
    }
    let action = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| "expected plan, apply, or recover".to_string())?;
    if action == "recover" {
        return run_recover(&args[1..]);
    }
    let mut repo = PathBuf::from(".");
    let mut mode = None;
    let mut confirm = None;
    let mut resolution_file = None;
    let mut json = false;
    let mut diff = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--repo" => {
                repo = PathBuf::from(args.get(index + 1).ok_or("--repo requires a path")?);
                index += 2;
            }
            "--local-only" => {
                mode = Some(RepositoryMode::LocalOnly);
                index += 1;
            }
            "--confirm" => {
                confirm = Some(
                    args.get(index + 1)
                        .ok_or("--confirm requires a digest")?
                        .clone(),
                );
                index += 2;
            }
            "--resolution-file" => {
                resolution_file = Some(PathBuf::from(
                    args.get(index + 1)
                        .ok_or("--resolution-file requires a path")?,
                ));
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
            option => return Err(format!("unknown option {option}")),
        }
    }
    if diff && json {
        return Err("--diff and --json are separate review formats; choose one".into());
    }
    let mut migration_diff = None;
    let report = match action {
        "plan" => {
            let built = build_plan(&repo, mode, resolution_file.as_deref(), None)?;
            if diff {
                migration_diff = Some(built.migration_diff);
            }
            built.report
        }
        "apply" => {
            if diff {
                return Err("--diff is available only for upgrade plan".into());
            }
            apply_with_resolution_file(
                &repo,
                mode,
                confirm
                    .as_deref()
                    .ok_or("apply requires --confirm <plan-sha256>")?,
                resolution_file.as_deref(),
            )?
        }
        other => {
            return Err(format!(
                "unknown action {other}; expected plan, apply, or recover"
            ));
        }
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        );
    } else {
        println!(
            "Repository schema: {} -> {}",
            report.from_schema, report.to_schema
        );
        println!("Mode: {:?}", report.mode);
        println!("Applied: {}", report.applied);
        println!("Source HEAD: {}", report.repository_head);
        println!(
            "Existing managed state: {}",
            report.existing_managed_state_digest
        );
        println!("Migration diff SHA-256: {}", report.diff_sha256);
        println!("Plan SHA-256: {}", report.plan_digest);
        for migration in &report.migrations {
            println!("  migration: {migration}");
        }
        for path in &report.dirty_paths {
            println!("  dirty path: {path}");
        }
        for path in &report.overlapping_dirty_paths {
            println!("  overlapping dirty path: {path}");
        }
        for path in &report.disjoint_dirty_paths {
            println!("  disjoint dirty path: {path}");
        }
        for session in &report.active_sessions {
            println!(
                "  live session: {} ({:?})",
                session.session_id, session.status
            );
        }
        for lease in &report.relevant_leases {
            println!(
                "  relevant lease: {} by session {} ({:?}, overlaps {})",
                lease.path,
                lease.session_id,
                lease.kind,
                lease.overlapping_planned_paths.join(", ")
            );
        }
        for warning in &report.warnings {
            println!("  warning: {warning}");
        }
        for blocker in &report.blockers {
            println!("  blocker: {blocker}");
        }
        for customization in &report.customizations {
            println!(
                "  policy: {} ({:?}{})",
                customization.path,
                customization.classification,
                customization
                    .resolution
                    .map(|choice| format!(", resolution: {choice:?}"))
                    .unwrap_or_default()
            );
        }
        println!("Next: {}", report.next_action);
        if let Some(migration_diff) = migration_diff {
            println!("Migration diff:");
            if migration_diff.is_empty() {
                println!("(no changes)");
            } else {
                print!("{migration_diff}");
            }
        }
    }
    Ok(())
}

fn run_recover(args: &[String]) -> Result<(), String> {
    let mut repo = PathBuf::from(".");
    let mut plan_digest = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--repo" => {
                repo = PathBuf::from(args.get(index + 1).ok_or("--repo requires a path")?);
                index += 2;
            }
            "--plan" => {
                plan_digest = Some(
                    args.get(index + 1)
                        .ok_or("--plan requires a digest")?
                        .clone(),
                );
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            option => return Err(format!("unknown recover option {option}")),
        }
    }

    let recovery = recover(
        &repo,
        plan_digest
            .as_deref()
            .ok_or("recover requires --plan <plan-sha256>")?,
    )?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&recovery).map_err(|error| error.to_string())?
        );
    } else {
        println!("Recovered plan: {}", recovery.plan_digest);
        for path in &recovery.restored_paths {
            println!("  restored: {path}");
        }
        println!("Next: {}", recovery.next_action);
    }
    Ok(())
}

fn print_usage() {
    println!("Usage:");
    println!(
        "  aethyme upgrade plan [--repo <path>] [--local-only] [--resolution-file <path>] [--diff|--json]"
    );
    println!(
        "  aethyme upgrade apply [--repo <path>] [--local-only] [--resolution-file <path>] --confirm <plan-sha256> [--json]"
    );
    println!("  aethyme upgrade recover [--repo <path>] --plan <plan-sha256> [--json]");
}

#[cfg(test)]
mod compatibility_tests {
    use std::process::Command;

    use super::{
        CommandCapability, CompatibilityContext, CompatibilityExecution, CompatibilitySeverity,
        InvocationSurface, MIGRATION_ID, MIGRATION_IN_PROGRESS, REPOSITORY_SCHEMA_VERSION,
        RepositoryCompatibility, RepositoryMarker, acquire_upgrade_lock, classify_marker,
        decide_compatibility, initial_enrollment_plan,
    };

    #[test]
    fn first_enrollment_is_planned_from_schema_zero_without_writing() {
        let temporary = tempfile::tempdir().unwrap();
        let repo = temporary.path().join("repo");
        let init = Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .arg(&repo)
            .status()
            .unwrap();
        assert!(init.success());
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        for args in [
            vec!["add", "README.md"],
            vec![
                "-c",
                "user.name=Aethyme Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "initial",
            ],
        ] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&repo)
                    .status()
                    .unwrap()
                    .success()
            );
        }

        let before = Command::new("git")
            .args(["status", "--short"])
            .current_dir(&repo)
            .output()
            .unwrap();
        let plan = initial_enrollment_plan(&repo, None).unwrap();
        assert_eq!(plan.from_schema, 0);
        assert!(plan.safe, "{:?}", plan.blockers);
        assert!(plan.planned_paths.iter().any(|path| path == "AGENTS.md"));
        assert!(
            plan.planned_paths
                .iter()
                .any(|path| path == ".aethyme/repository.json")
        );
        let after = Command::new("git")
            .args(["status", "--short"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert_eq!(before.stdout, after.stdout);
    }

    #[cfg(unix)]
    #[test]
    fn repository_upgrade_lock_is_exclusive_and_reusable() {
        let temp = tempfile::tempdir().unwrap();
        let initialized = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .arg(temp.path())
            .output()
            .unwrap();
        assert!(initialized.status.success());

        let first = acquire_upgrade_lock(temp.path()).unwrap();
        let second = acquire_upgrade_lock(temp.path());
        assert!(
            matches!(second, Err(error) if error.contains("exclusive upgrade lock")),
            "a second upgrade unexpectedly acquired the repository lock"
        );
        drop(first);
        assert!(acquire_upgrade_lock(temp.path()).is_ok());
    }

    fn schema(repository: RepositoryCompatibility) -> Option<u32> {
        match repository {
            RepositoryCompatibility::Current => Some(REPOSITORY_SCHEMA_VERSION),
            RepositoryCompatibility::UpgradeRequired
            | RepositoryCompatibility::UpgradeInProgress => Some(0),
            RepositoryCompatibility::NewerThanBinary => Some(REPOSITORY_SCHEMA_VERSION + 1),
            RepositoryCompatibility::Invalid => None,
        }
    }

    #[test]
    fn marker_classification_covers_every_non_invalid_repository_state() {
        let current = RepositoryMarker {
            schema_version: REPOSITORY_SCHEMA_VERSION,
            applied_migrations: vec![MIGRATION_ID.into()],
        };
        let newer = RepositoryMarker {
            schema_version: REPOSITORY_SCHEMA_VERSION + 1,
            applied_migrations: vec![MIGRATION_ID.into()],
        };
        let in_progress = RepositoryMarker {
            schema_version: 0,
            applied_migrations: vec![MIGRATION_IN_PROGRESS.into()],
        };
        let incomplete = RepositoryMarker {
            schema_version: REPOSITORY_SCHEMA_VERSION,
            applied_migrations: Vec::new(),
        };

        assert_eq!(
            classify_marker(Some(&current)),
            RepositoryCompatibility::Current
        );
        assert_eq!(
            classify_marker(Some(&newer)),
            RepositoryCompatibility::NewerThanBinary
        );
        assert_eq!(
            classify_marker(Some(&in_progress)),
            RepositoryCompatibility::UpgradeInProgress
        );
        assert_eq!(
            classify_marker(Some(&incomplete)),
            RepositoryCompatibility::UpgradeRequired
        );
        assert_eq!(
            classify_marker(None),
            RepositoryCompatibility::UpgradeRequired
        );
    }

    #[test]
    fn command_lanes_preserve_diagnostics_and_recovery_without_new_mutations() {
        let repository_states = [
            RepositoryCompatibility::Current,
            RepositoryCompatibility::UpgradeRequired,
            RepositoryCompatibility::NewerThanBinary,
            RepositoryCompatibility::Invalid,
            RepositoryCompatibility::UpgradeInProgress,
        ];
        let capabilities = [
            CommandCapability::DiagnosticRead,
            CommandCapability::RecoveryWrite,
            CommandCapability::SessionContinuation,
            CommandCapability::ManagedPreCommit,
            CommandCapability::NewSession,
            CommandCapability::SharedMutation,
            CommandCapability::Upgrade,
        ];

        for repository in repository_states {
            for capability in capabilities {
                let decision = decide_compatibility(
                    repository,
                    schema(repository),
                    capability,
                    CompatibilityContext::default(),
                    "reason".into(),
                    Some("remediation".into()),
                );
                let expected = match repository {
                    RepositoryCompatibility::Current => true,
                    RepositoryCompatibility::UpgradeRequired
                    | RepositoryCompatibility::UpgradeInProgress => matches!(
                        capability,
                        CommandCapability::DiagnosticRead
                            | CommandCapability::RecoveryWrite
                            | CommandCapability::Upgrade
                    ),
                    RepositoryCompatibility::NewerThanBinary | RepositoryCompatibility::Invalid => {
                        matches!(
                            capability,
                            CommandCapability::DiagnosticRead | CommandCapability::Upgrade
                        )
                    }
                };
                assert_eq!(
                    decision.allowed, expected,
                    "unexpected decision for {repository:?} and {capability:?}"
                );
                assert_eq!(decision.repository, repository);
                assert_eq!(decision.capability, capability);
                assert_eq!(decision.refusal_message().is_none(), decision.allowed);
                assert_eq!(
                    decision.execution,
                    if expected
                        && repository != RepositoryCompatibility::Current
                        && capability == CommandCapability::DiagnosticRead
                    {
                        CompatibilityExecution::ReadOnlySnapshot
                    } else {
                        CompatibilityExecution::Normal
                    }
                );
            }
        }
    }

    #[test]
    fn only_a_matching_pinned_contract_can_continue_a_backward_executable_migration() {
        let legacy = aethyme_broker::RepositoryContract {
            repository_schema: None,
            deployment_state_digest: "a".repeat(64),
            aethyme_version: "0.1.0".into(),
            gate_definition_digest: None,
            backfilled: true,
        };
        let current = aethyme_broker::RepositoryContract {
            repository_schema: Some(REPOSITORY_SCHEMA_VERSION),
            ..legacy.clone()
        };
        for capability in [
            CommandCapability::SessionContinuation,
            CommandCapability::ManagedPreCommit,
        ] {
            for (contract, expected) in [
                (Some(&legacy), true),
                (Some(&current), false),
                (None, false),
            ] {
                let decision = decide_compatibility(
                    RepositoryCompatibility::UpgradeRequired,
                    Some(0),
                    capability,
                    CompatibilityContext {
                        session_contract: contract,
                        ..CompatibilityContext::default()
                    },
                    "reason".into(),
                    Some("remediation".into()),
                );
                assert_eq!(decision.allowed, expected);
            }
        }
    }

    #[test]
    fn managed_pre_commit_refusal_preserves_dirty_work_context() {
        let decision = decide_compatibility(
            RepositoryCompatibility::UpgradeRequired,
            Some(0),
            CommandCapability::ManagedPreCommit,
            CompatibilityContext::default(),
            "reason".into(),
            Some("remediation".into()),
        );
        assert_eq!(
            decision.managed_pre_commit_refusal_message().as_deref(),
            Some(
                "git commit refused by Aethyme pre-commit:\n\
                 repository deployment schema 0 must be upgraded to schema 1.\n\
                 Your changes remain in the worktree."
            )
        );
    }

    #[test]
    fn refusal_text_names_the_invoking_surface_and_legal_recovery_lane() {
        for (surface, prefix) in [
            (InvocationSurface::BrokerCommand, "broker command refused"),
            (
                InvocationSurface::CoordinatedOperation,
                "coordinated operation refused",
            ),
        ] {
            let decision = decide_compatibility(
                RepositoryCompatibility::UpgradeRequired,
                Some(0),
                CommandCapability::SharedMutation,
                CompatibilityContext {
                    surface,
                    ..CompatibilityContext::default()
                },
                "repository deployment requires an embedded upgrade".into(),
                None,
            );
            let refusal = decision.refusal_message().unwrap();
            assert!(refusal.starts_with(prefix), "{refusal}");
            assert!(refusal.contains("managed pre-commit lane"), "{refusal}");
            assert!(
                refusal.contains("broker finish --session <id>"),
                "{refusal}"
            );
            assert!(!refusal.contains("stash"), "{refusal}");
        }
    }

    #[test]
    fn severity_describes_repository_state_independently_of_allowance() {
        let cases = [
            (
                RepositoryCompatibility::Current,
                CompatibilitySeverity::Info,
            ),
            (
                RepositoryCompatibility::UpgradeRequired,
                CompatibilitySeverity::Warning,
            ),
            (
                RepositoryCompatibility::UpgradeInProgress,
                CompatibilitySeverity::Warning,
            ),
            (
                RepositoryCompatibility::NewerThanBinary,
                CompatibilitySeverity::Error,
            ),
            (
                RepositoryCompatibility::Invalid,
                CompatibilitySeverity::Error,
            ),
        ];
        for (repository, severity) in cases {
            let decision = decide_compatibility(
                repository,
                schema(repository),
                CommandCapability::Upgrade,
                CompatibilityContext::default(),
                "reason".into(),
                None,
            );
            assert!(decision.allowed);
            assert_eq!(decision.severity, severity);
        }
    }
}

#[cfg(test)]
mod proposed_tree_tests {
    use super::*;

    fn entry(digest: &str, mode: &str) -> Option<TreeEntry> {
        Some(TreeEntry {
            sha256: digest.into(),
            file_mode: mode.into(),
        })
    }

    #[test]
    fn change_classification_covers_every_action_and_resolution_boundary() {
        let paths = vec![
            ".aethyme/config.toml".to_string(),
            "create".to_string(),
            "delete".to_string(),
            "unchanged".to_string(),
        ];
        let before = BTreeMap::from([
            (".aethyme/config.toml".into(), entry("old", "100644")),
            ("create".into(), None),
            ("delete".into(), entry("old", "100755")),
            ("unchanged".into(), entry("same", "100644")),
        ]);
        let after = BTreeMap::from([
            (".aethyme/config.toml".into(), entry("new", "100644")),
            ("create".into(), entry("new", "100644")),
            ("delete".into(), None),
            ("unchanged".into(), entry("same", "100644")),
        ]);

        let changes = classify_tree_changes(
            RepositoryMode::Canonical,
            &paths,
            &before,
            &after,
            &BTreeSet::new(),
        );
        assert_eq!(
            changes
                .iter()
                .map(|change| change.action)
                .collect::<Vec<_>>(),
            vec![
                RepositoryTreeAction::Update,
                RepositoryTreeAction::Create,
                RepositoryTreeAction::Delete,
                RepositoryTreeAction::Unchanged,
            ]
        );
        assert!(changes[0].requires_resolution);
        assert_eq!(
            changes[0].ownership,
            RepositoryPathOwnership::RepositoryOwned
        );
        assert_eq!(changes[2].file_mode.as_deref(), Some("100755"));
        assert!(!changes[3].requires_resolution);
    }
}
