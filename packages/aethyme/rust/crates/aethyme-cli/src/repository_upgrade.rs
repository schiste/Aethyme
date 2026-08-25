//! Embedded, versioned migrations for repository deployment artifacts.
//!
//! The binary update and repository update are deliberately separate. A
//! package manager has no trustworthy repository scope; the first broker use
//! in an enrolled repository refuses an old schema and points here instead.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const REPOSITORY_SCHEMA_VERSION: u32 = aethyme_broker::REPOSITORY_SCHEMA_VERSION;
pub const CANONICAL_MARKER_PATH: &str = aethyme_broker::CANONICAL_REPOSITORY_MARKER_PATH;
pub const LOCAL_MARKER_PATH: &str = aethyme_broker::LOCAL_REPOSITORY_MARKER_PATH;
const PLAN_SCHEMA_VERSION: u32 = 1;
const MIGRATION_ID: &str = "repository-deployment-v1";
const MIGRATION_IN_PROGRESS: &str = "repository-deployment-v1:in-progress";

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
    pub state_digest: String,
    pub migrations: Vec<String>,
    pub planned_paths: Vec<String>,
    pub applied: bool,
    pub safe: bool,
    pub blockers: Vec<String>,
    pub plan_digest: String,
    pub next_action: String,
}

#[derive(Serialize)]
struct PlanDigest<'a> {
    schema_version: u32,
    mode: RepositoryMode,
    from_schema: u32,
    to_schema: u32,
    repository_head: &'a str,
    state_digest: &'a str,
    migrations: &'a [String],
    planned_paths: &'a [String],
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
    NewSession,
    SharedMutation,
    Upgrade,
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
}

/// A pure, render-independent compatibility decision for one parsed command.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CompatibilityDecision {
    pub repository: RepositoryCompatibility,
    pub capability: CommandCapability,
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
        Some(match &self.remediation {
            Some(remediation) => format!("{}; {remediation}", self.reason),
            None => self.reason.clone(),
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
                CommandCapability::SessionContinuation => pinned_session_is_executable,
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
    CompatibilityDecision {
        repository,
        capability,
        allowed,
        severity,
        execution,
        reason,
        remediation,
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

pub fn plan(
    repo_hint: &Path,
    requested_mode: Option<RepositoryMode>,
) -> Result<RepositoryUpgradePlan, String> {
    let repo = git_root(repo_hint)?;
    let detected = detect_mode(&repo);
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
    let marker = match read_marker(&repo, mode) {
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
    for relative in managed_paths(mode) {
        if let Some(blocker) = managed_path_blocker(&repo, &relative)? {
            blockers.push(blocker);
        }
    }
    let status = git(&repo, &["status", "--porcelain", "--untracked-files=all"])?;
    if !status.trim().is_empty() {
        blockers.push("repository worktree is dirty; commit or stash changes before applying a repository upgrade".into());
    }
    let repository_head = git(&repo, &["rev-parse", "HEAD"])?;
    let migrations = EMBEDDED_MIGRATIONS
        .iter()
        .filter(|migration| {
            migration.from_schema >= from_schema && migration.to_schema <= REPOSITORY_SCHEMA_VERSION
        })
        .map(|migration| migration.id.to_string())
        .collect::<Vec<_>>();
    let planned_paths = if migrations.is_empty() {
        Vec::new()
    } else {
        managed_paths(mode)
    };
    let state_digest = repository_state_digest(&repo, mode)?;
    let safe = blockers.is_empty();
    let digest_input = PlanDigest {
        schema_version: PLAN_SCHEMA_VERSION,
        mode,
        from_schema,
        to_schema: REPOSITORY_SCHEMA_VERSION,
        repository_head: &repository_head,
        state_digest: &state_digest,
        migrations: &migrations,
        planned_paths: &planned_paths,
        safe,
        blockers: &blockers,
    };
    let plan_digest =
        sha256(&serde_json::to_vec(&digest_input).map_err(|error| error.to_string())?);
    let next_action = if !safe {
        "resolve every blocker, then regenerate the upgrade plan".into()
    } else if migrations.is_empty() {
        "repository deployment is current; no migration is required".into()
    } else {
        format!(
            "review this plan, then run `aethyme upgrade apply --repo . --confirm {plan_digest}`"
        )
    };
    Ok(RepositoryUpgradePlan {
        schema_version: PLAN_SCHEMA_VERSION,
        mode,
        from_schema,
        to_schema: REPOSITORY_SCHEMA_VERSION,
        repository_head,
        state_digest,
        migrations,
        planned_paths,
        applied: false,
        safe,
        blockers,
        plan_digest,
        next_action,
    })
}

pub fn apply(
    repo_hint: &Path,
    requested_mode: Option<RepositoryMode>,
    confirmation: &str,
) -> Result<RepositoryUpgradePlan, String> {
    if confirmation.len() != 64 || !confirmation.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("--confirm must be the full 64-character plan SHA-256".into());
    }
    let mut before = plan(repo_hint, requested_mode)?;
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

    let repo = git_root(repo_hint)?;
    write_pending_marker(&repo, before.mode)?;
    match before.mode {
        RepositoryMode::Canonical => migrate_canonical(&repo)?,
        RepositoryMode::LocalOnly => migrate_local(&repo)?,
    }
    write_current_marker(&repo, before.mode)?;
    verify_deployment(&repo, before.mode)?;
    before.applied = true;
    before.state_digest = repository_state_digest(&repo, before.mode)?;
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

fn migrate_canonical(repo: &Path) -> Result<(), String> {
    aethyme_broker::init::scaffold(repo).map_err(|error| error.to_string())?;
    if !repo.join(".aethyme/gates.toml").exists() {
        aethyme_broker::init::draft_gates(repo).map_err(|error| error.to_string())?;
    }
    // A migration converges managed artifacts to the new binary's embedded
    // contract. A normal deploy preserves existing files unless --force was
    // requested, which is the wrong semantic for a reviewed schema upgrade.
    aethyme_enhance::deploy::deploy(repo, true)?;
    Ok(())
}

fn migrate_local(repo: &Path) -> Result<(), String> {
    aethyme_enhance::local::prepare(repo)?;
    aethyme_broker::init::scaffold_local(repo).map_err(|error| error.to_string())?;
    if !repo.join(".aethyme/gates.toml").exists() {
        aethyme_broker::init::draft_gates(repo).map_err(|error| error.to_string())?;
    }
    aethyme_enhance::local::deploy(repo, true)?;
    Ok(())
}

fn verify_deployment(repo: &Path, mode: RepositoryMode) -> Result<(), String> {
    verify_current_marker(repo, mode)?;
    match mode {
        RepositoryMode::Canonical => {
            let results = aethyme_enhance::deploy::verify(repo)?;
            let failures = results
                .iter()
                .filter(|result| {
                    !result.exists
                        || result.placeholder_present
                        || (!result.matches_canonical
                            && matches!(result.relative_path.as_str(), "AGENTS.md" | "CLAUDE.md"))
                })
                .map(|result| result.relative_path.clone())
                .collect::<Vec<_>>();
            if !failures.is_empty() {
                return Err(format!(
                    "post-migration verification failed: {}",
                    failures.join(", ")
                ));
            }
        }
        RepositoryMode::LocalOnly => {
            let failures = aethyme_enhance::local::verify(repo)?;
            if !failures.is_empty() {
                return Err(format!(
                    "post-migration verification failed: {}",
                    failures.join("; ")
                ));
            }
        }
    }
    Ok(())
}

fn detect_mode(repo: &Path) -> Option<RepositoryMode> {
    if repo
        .join(aethyme_enhance::local::LOCAL_MARKER_PATH)
        .is_file()
        || repo.join(LOCAL_MARKER_PATH).is_file()
    {
        Some(RepositoryMode::LocalOnly)
    } else if std::fs::read_to_string(repo.join("AGENTS.md"))
        .map(|text| text.contains("Broker Coordination"))
        .unwrap_or(false)
    {
        Some(RepositoryMode::Canonical)
    } else {
        None
    }
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
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
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
        .ok_or_else(|| "expected plan or apply".to_string())?;
    let mut repo = PathBuf::from(".");
    let mut mode = None;
    let mut confirm = None;
    let mut json = false;
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
            "--json" => {
                json = true;
                index += 1;
            }
            option => return Err(format!("unknown option {option}")),
        }
    }
    let report = match action {
        "plan" => plan(&repo, mode)?,
        "apply" => apply(
            &repo,
            mode,
            confirm
                .as_deref()
                .ok_or("apply requires --confirm <plan-sha256>")?,
        )?,
        other => return Err(format!("unknown action {other}; expected plan or apply")),
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
        println!("Plan SHA-256: {}", report.plan_digest);
        for migration in &report.migrations {
            println!("  migration: {migration}");
        }
        for blocker in &report.blockers {
            println!("  blocker: {blocker}");
        }
        println!("Next: {}", report.next_action);
    }
    Ok(())
}

fn print_usage() {
    println!("Usage:");
    println!("  aethyme upgrade plan [--repo <path>] [--local-only] [--json]");
    println!(
        "  aethyme upgrade apply [--repo <path>] [--local-only] --confirm <plan-sha256> [--json]"
    );
}

#[cfg(test)]
mod compatibility_tests {
    use super::{
        CommandCapability, CompatibilityContext, CompatibilityExecution, CompatibilitySeverity,
        MIGRATION_ID, MIGRATION_IN_PROGRESS, REPOSITORY_SCHEMA_VERSION, RepositoryCompatibility,
        RepositoryMarker, classify_marker, decide_compatibility,
    };

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
        for (contract, expected) in [
            (Some(&legacy), true),
            (Some(&current), false),
            (None, false),
        ] {
            let decision = decide_compatibility(
                RepositoryCompatibility::UpgradeRequired,
                Some(0),
                CommandCapability::SessionContinuation,
                CompatibilityContext {
                    session_contract: contract,
                },
                "reason".into(),
                Some("remediation".into()),
            );
            assert_eq!(decision.allowed, expected);
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
