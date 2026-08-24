//! Embedded, versioned migrations for repository deployment artifacts.
//!
//! The binary update and repository update are deliberately separate. A
//! package manager has no trustworthy repository scope; the first broker use
//! in an enrolled repository refuses an old schema and points here instead.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const REPOSITORY_SCHEMA_VERSION: u32 = aethyme_broker::REPOSITORY_SCHEMA_VERSION;
pub const CANONICAL_MARKER_PATH: &str = ".aethyme/repository.json";
pub const LOCAL_MARKER_PATH: &str = ".aethyme/local/repository.json";
const PLAN_SCHEMA_VERSION: u32 = 1;
const MIGRATION_ID: &str = "repository-deployment-v1";
const MIGRATION_IN_PROGRESS: &str = "repository-deployment-v1:in-progress";

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

pub fn compatibility_blocker(repo_hint: &Path) -> Option<String> {
    let repo = git_root(repo_hint).ok()?;
    let mode = detect_mode(&repo)?;
    match read_marker(&repo, mode) {
        Ok(Some(marker)) if marker_is_current(&marker) => None,
        Ok(Some(marker)) if marker.schema_version > REPOSITORY_SCHEMA_VERSION => Some(format!(
            "repository schema {} is newer than this binary supports ({}); update Aethyme before using broker commands",
            marker.schema_version, REPOSITORY_SCHEMA_VERSION
        )),
        Ok(_) => Some(format!(
            "repository deployment requires an embedded upgrade; run `aethyme upgrade plan --repo .`, review it, then `aethyme upgrade apply --repo . --confirm <plan-sha256>`"
        )),
        Err(error) => Some(format!(
            "repository deployment marker is invalid: {error}; inspect {} and run `aethyme upgrade plan --repo .`",
            mode.marker_path()
        )),
    }
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
    let migrations = if from_schema < REPOSITORY_SCHEMA_VERSION {
        vec![MIGRATION_ID.into()]
    } else {
        Vec::new()
    };
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
    let mut paths = BTreeSet::new();
    paths.extend([
        ".aethyme/config.toml".into(),
        ".aethyme/gates.toml".into(),
        mode.marker_path().into(),
    ]);
    match mode {
        RepositoryMode::Canonical => {
            paths.insert(".gitignore".into());
            paths.insert("AGENTS.md".into());
            paths.insert("CLAUDE.md".into());
            paths.extend(
                aethyme_enhance::deploy::TARGETS
                    .iter()
                    .map(|(path, _)| (*path).into()),
            );
        }
        RepositoryMode::LocalOnly => {
            paths.insert(aethyme_enhance::local::LOCAL_MARKER_PATH.into());
            paths.insert(aethyme_enhance::local::LOCAL_POLICY_PATH.into());
            paths.extend(
                aethyme_enhance::deploy::TARGETS
                    .iter()
                    .filter(|(path, _)| *path != "CLAUDE.md")
                    .map(|(path, _)| (*path).into()),
            );
        }
    }
    paths.insert(aethyme_enhance::deploy::SETTINGS_FILE.into());
    paths.extend([
        aethyme_enhance::AGENTS_OVERRIDE_PATH.into(),
        aethyme_enhance::onboarding::ONBOARDING_JSON_PATH.into(),
        aethyme_enhance::onboarding::ACT_STARTER_JSON_PATH.into(),
        aethyme_enhance::onboarding::ONBOARDING_CLAUDE_PATH.into(),
        aethyme_enhance::onboarding::ONBOARDING_CODEX_PATH.into(),
        aethyme_enhance::onboarding::ACT_CLAUDE_PATH.into(),
        aethyme_enhance::onboarding::ACT_CODEX_PATH.into(),
    ]);
    paths.into_iter().collect()
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
    let mut hasher = Sha256::new();
    for relative in managed_paths(mode) {
        hasher.update(relative.as_bytes());
        let path = repo.join(&relative);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => hasher.update(b"symlink"),
            Ok(metadata) if metadata.is_file() => {
                hasher.update(b"file");
                hasher
                    .update(std::fs::read(&path).map_err(|error| format!("{relative}: {error}"))?);
            }
            Ok(_) => hasher.update(b"other"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => hasher.update(b"missing"),
            Err(error) => return Err(format!("{relative}: {error}")),
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
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
