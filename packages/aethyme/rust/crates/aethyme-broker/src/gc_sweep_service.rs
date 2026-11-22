//! Opt-in, per-repository macOS scheduling for the public fixed GC sweep.
//!
//! The service plan binds the immutable router and engine pair selected by the
//! managed installer. The LaunchAgent only invokes the public `gc sweep`
//! command; native GC policy, admission, singleton and retry state stay in the
//! broker.

use crate::scheduled_service::{
    absolute_directory, binary_digest, digest, executable, invalid, text, xml,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PLAN_SCHEMA: &str = "aethyme.gc-sweep-service-plan/v1";
const CONTROL_SCHEMA: &str = "aethyme.gc-sweep-service-control/v1";
const RECEIPT_SCHEMA: &str = "aethyme.gc-sweep-service-last-pass/v1";
const REFRESH_SCHEMA: &str = "aethyme.gc-sweep-service-refresh/v1";
const ERROR_SCHEMA: &str = "aethyme.gc-sweep-service-last-error/v1";
/// The command that rebinds this repository's service to the running runtime.
pub(crate) const REFRESH_COMMAND: &str = "aethyme broker gc service refresh";
const SERVICE_DIRECTORY: &str = "Library/Application Support/Aethyme/gc-sweep-services";
const UNIT_DIRECTORY: &str = "Library/LaunchAgents";
const SERVICE_ENV_UUID: &str = "AETHYME_GC_SWEEP_SERVICE_UUID";
const SERVICE_ENV_GENERATION: &str = "AETHYME_GC_SWEEP_SERVICE_GENERATION";
const SERVICE_INTERVAL_SECONDS: u64 = 900;
const MAX_RECEIPT_BYTES: usize = 4096;
const VERSION_PROBE_BYTES: usize = 4096;
const VERSION_PROBE_BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Plan {
    pub schema: String,
    pub repository_uuid: String,
    pub repository: PathBuf,
    pub home: PathBuf,
    pub uid: u32,
    pub managed_root: PathBuf,
    pub version_directory: PathBuf,
    pub runtime_version: String,
    pub runtime_build_identity: String,
    pub router: PathBuf,
    pub engine: PathBuf,
    pub router_sha256: String,
    pub engine_sha256: String,
    pub directory: PathBuf,
    pub receipt_path: PathBuf,
    pub label: String,
    pub unit_path: PathBuf,
    pub interval_seconds: u64,
    pub generation: String,
    pub unit: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimePair {
    managed_root: PathBuf,
    version_directory: PathBuf,
    runtime_version: String,
    runtime_build_identity: String,
    router: PathBuf,
    engine: PathBuf,
    router_sha256: String,
    engine_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ControlReceipt {
    schema: String,
    plan_digest: String,
    generation: String,
    action: String,
    scheduler_state: String,
    operation_result: String,
    observed_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LastPassReceipt {
    schema: String,
    repository_uuid: String,
    generation: String,
    started_at_unix_ms: u64,
    finished_at_unix_ms: u64,
    outcome: String,
    directories_reclaimed: Option<usize>,
    budget_ms: Option<u64>,
    disk_pressured: Option<bool>,
    legacy_unscoped_state: Option<String>,
    error: Option<String>,
    // Per-lane fields, added within v1: absent from older receipts, and
    // ignored by older readers (no `deny_unknown_fields`).
    /// `ran`, or why the artifact cache lane returned at once
    /// ([`crate::ArtifactSweepLane`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artifact_lane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entries_reclaimed: Option<usize>,
    /// Logical file bytes observed before unlink plus preparation-cache bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bytes_reclaimed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkouts_removed: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkouts_kept: Option<usize>,
    /// The trash as this pass found it (the last observation on a failed
    /// pass).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trash: Option<crate::CheckoutTrashBacklog>,
    /// Entries the paced deleter removed from the deletable backlog since
    /// the previous pass (a floor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trash_entries_drained: Option<usize>,
    /// Entries that left the deletable backlog since the previous pass
    /// without being deleted: their identity changed, they reached the
    /// failure cap, or a process uses them. Never counted as drained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trash_entries_held_back: Option<usize>,
    /// Set when the pass reclaimed nothing: the one cause that explains it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reclaimed_nothing_because: Option<String>,
    /// Consecutive passes, this one included, that reclaimed nothing.
    #[serde(default)]
    passes_without_reclaim: u64,
    /// When the first of those passes started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unreclaimed_since_unix_ms: Option<u64>,
}

/// Consecutive passes that reclaimed nothing before `broker status` says so
/// (two hours at the fixed 900 s interval).
const UNRECLAIMED_PASSES_ADVICE_THRESHOLD: u64 = 8;

/// What the paced host deleter did to the confirmed trash between the
/// previous pass and this one. It deletes the oldest entries first, so a
/// backlog that shrank (net of this pass's confirmations, and of entries
/// held back without being deleted) or whose oldest entry moved forward
/// while nothing was held back is draining. `None` without a previous
/// observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TrashDrain {
    /// Entries deleted from the deletable backlog since the previous pass
    /// (a floor: entries trashed meanwhile hide some).
    entries: usize,
    /// Entries that left the deletable backlog without being deleted
    /// (identity changed, failure cap, in use) since the previous pass.
    held_back: usize,
    draining: bool,
}

fn trash_drain_since(
    previous: Option<&crate::CheckoutTrashBacklog>,
    current: &crate::CheckoutTrashBacklog,
) -> Option<TrashDrain> {
    let previous = previous?;
    // Entries newly held back left the deletable count too; they are not
    // the deleter's work. (A floor: one released meanwhile hides one.)
    let held_back = current.held_back().saturating_sub(previous.held_back());
    let entries = previous
        .deletable
        .saturating_add(current.confirmed_this_pass)
        .saturating_sub(current.deletable)
        .saturating_sub(held_back);
    let oldest_moved = match (
        previous.oldest_deletable_created_ms,
        current.oldest_deletable_created_ms,
    ) {
        (Some(before), Some(now)) => now > before,
        (Some(_), None) => true,
        _ => false,
    };
    Some(TrashDrain {
        entries,
        held_back,
        // The oldest entry may have moved only because it was held back.
        draining: entries > 0 || (oldest_moved && held_back == 0),
    })
}

/// Whether a pass reclaimed or confirmed anything, counting what the paced
/// deleter removed from the trash since the previous pass.
fn pass_reclaimed(report: &crate::ArtifactSweepReport, drain: Option<TrashDrain>) -> bool {
    report.directories_reclaimed > 0
        || report.entries_reclaimed > 0
        || report.observed_logical_file_bytes > 0
        || report.preparation_cache_bytes_reclaimed > 0
        || report.checkout_trash.confirmed_this_pass > 0
        || drain.is_some_and(|drain| drain.draining)
        || report
            .auto_cleanup
            .as_ref()
            .is_some_and(|cleanup| !cleanup.removed.is_empty())
}

/// The one cause that explains a pass that reclaimed nothing: a stalled
/// deleter first (actionable), then the lane that actually blocked, then
/// trash rows no command can help. `None` when nothing reclaimable was in
/// view, or a trash backlog had no previous observation to compare with.
fn reclaimed_nothing_because(
    report: &crate::ArtifactSweepReport,
    drain: Option<TrashDrain>,
) -> Option<&'static str> {
    let trash = &report.checkout_trash;
    if trash.deletable > 0 && drain.is_some_and(|drain| !drain.draining) {
        Some("trash_not_draining")
    } else if report.artifact_lane == crate::ArtifactSweepLane::GcLockHeld {
        Some("artifact_lane_yielded_to_worker")
    } else if report.artifact_lane == crate::ArtifactSweepLane::Disabled {
        Some("artifact_lane_disabled")
    } else if report.artifact_lane == crate::ArtifactSweepLane::PausedByStrictCleanup {
        Some("artifact_lane_paused_by_strict_cleanup")
    } else if !report.complete {
        Some("artifact_lane_budget_spent")
    } else if trash.stale_pending > 0 {
        // Kept by the proofs (identity changed, session in use, trash not
        // readable as recorded): nothing to run, so never advised.
        Some("trash_entries_kept_by_proofs")
    } else if trash.identity_changed > 0 {
        // Confirmed entries present with another identity: never deleted;
        // `broker status` names them (`cleanup.trash-identity-changed`), and
        // no sweep or drain can act on them, so never advised here.
        Some("trash_entries_identity_changed")
    } else if trash.pending > 0 {
        Some("trash_entries_awaiting_mover")
    } else {
        None
    }
}

/// Kept alive for the complete managed tick so uninstall and control cannot
/// remove or change the generation while the sweep is running.
#[cfg(target_os = "macos")]
pub(crate) struct ManagedTick {
    plan: Plan,
    started_at_unix_ms: u64,
    _claim: crate::file_lock::ExclusiveFileLock,
}

impl Plan {
    fn from_runtime(
        repository: &Path,
        repository_uuid: &str,
        home: &Path,
        uid: u32,
        runtime: &RuntimePair,
    ) -> io::Result<Self> {
        crate::store::repository_identity::validate_uuid_v4(repository_uuid)
            .map_err(io::Error::other)?;
        if uid == 0 {
            return Err(invalid("GC sweep service requires a non-root user"));
        }
        let repository = absolute_directory(repository)?;
        let home = absolute_directory(home)?;
        let directory = home.join(SERVICE_DIRECTORY).join(repository_uuid);
        let receipt_path = directory.join("last-pass.json");
        let label = format!("com.aethyme.gc-sweep.{repository_uuid}");
        let unit_path = home.join(UNIT_DIRECTORY).join(format!("{label}.plist"));
        let mut plan = Self {
            schema: PLAN_SCHEMA.to_string(),
            repository_uuid: repository_uuid.to_string(),
            repository,
            home,
            uid,
            managed_root: runtime.managed_root.clone(),
            version_directory: runtime.version_directory.clone(),
            runtime_version: runtime.runtime_version.clone(),
            runtime_build_identity: runtime.runtime_build_identity.clone(),
            router: runtime.router.clone(),
            engine: runtime.engine.clone(),
            router_sha256: runtime.router_sha256.clone(),
            engine_sha256: runtime.engine_sha256.clone(),
            directory,
            receipt_path,
            label,
            unit_path,
            interval_seconds: SERVICE_INTERVAL_SECONDS,
            generation: String::new(),
            unit: String::new(),
            digest: String::new(),
        };
        plan.generation = generation(&plan)?;
        plan.unit = render_unit(&plan)?;
        plan.digest = digest(&serde_json::to_vec(&plan).map_err(io::Error::other)?);
        Ok(plan)
    }
}

fn generation(plan: &Plan) -> io::Result<String> {
    #[derive(Serialize)]
    struct Identity<'a> {
        schema: &'a str,
        repository_uuid: &'a str,
        repository: &'a Path,
        home: &'a Path,
        uid: u32,
        managed_root: &'a Path,
        version_directory: &'a Path,
        runtime_version: &'a str,
        runtime_build_identity: &'a str,
        router: &'a Path,
        engine: &'a Path,
        router_sha256: &'a str,
        engine_sha256: &'a str,
        directory: &'a Path,
        receipt_path: &'a Path,
        label: &'a str,
        unit_path: &'a Path,
        interval_seconds: u64,
    }
    let identity = Identity {
        schema: &plan.schema,
        repository_uuid: &plan.repository_uuid,
        repository: &plan.repository,
        home: &plan.home,
        uid: plan.uid,
        managed_root: &plan.managed_root,
        version_directory: &plan.version_directory,
        runtime_version: &plan.runtime_version,
        runtime_build_identity: &plan.runtime_build_identity,
        router: &plan.router,
        engine: &plan.engine,
        router_sha256: &plan.router_sha256,
        engine_sha256: &plan.engine_sha256,
        directory: &plan.directory,
        receipt_path: &plan.receipt_path,
        label: &plan.label,
        unit_path: &plan.unit_path,
        interval_seconds: plan.interval_seconds,
    };
    serde_json::to_vec(&identity)
        .map(|bytes| digest(&bytes))
        .map_err(io::Error::other)
}

fn render_unit(plan: &Plan) -> io::Result<String> {
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{}</string>\n<key>ProgramArguments</key><array><string>{}</string><string>broker</string><string>gc</string><string>sweep</string><string>--json</string></array>\n<key>WorkingDirectory</key><string>{}</string>\n<key>EnvironmentVariables</key><dict><key>HOME</key><string>{}</string><key>{SERVICE_ENV_UUID}</key><string>{}</string><key>{SERVICE_ENV_GENERATION}</key><string>{}</string></dict>\n<key>StartInterval</key><integer>{SERVICE_INTERVAL_SECONDS}</integer>\n<key>RunAtLoad</key><false/>\n<key>StandardOutPath</key><string>/dev/null</string>\n<key>StandardErrorPath</key><string>/dev/null</string>\n</dict></plist>\n",
        xml(&plan.label)?,
        xml(text(&plan.router)?)?,
        xml(text(&plan.repository)?)?,
        xml(text(&plan.home)?)?,
        xml(&plan.repository_uuid)?,
        xml(&plan.generation)?,
    ))
}

fn runtime_pair(current_executable: &Path) -> io::Result<RuntimePair> {
    use std::os::unix::fs::MetadataExt as _;

    let current = executable(current_executable)?;
    let installation = crate::update::detect_installation(&current);
    if installation.method != crate::update::InstallationMethod::Installer {
        return Err(invalid(
            "scheduled GC requires an installer-managed immutable runtime pair",
        ));
    }
    let managed_root = installation
        .managed_root
        .ok_or_else(|| invalid("managed runtime has no installation root"))?;
    let router = executable(&installation.router_path)?;
    let engine = executable(
        installation
            .engine_path
            .as_deref()
            .ok_or_else(|| invalid("managed runtime has no paired engine binary"))?,
    )?;
    if router != current
        || router.file_name().and_then(|n| n.to_str()) != Some("aethyme")
        || engine.file_name().and_then(|n| n.to_str()) != Some("aethyme-engine-cli")
        || router.parent() != engine.parent()
    {
        return Err(invalid(
            "managed router and engine do not resolve to the running versioned pair",
        ));
    }
    let version_directory = router
        .parent()
        .ok_or_else(|| invalid("managed runtime has no version directory"))?
        .to_path_buf();
    let versions_directory = managed_root.join("versions");
    if version_directory.parent() != Some(versions_directory.as_path())
        || version_directory
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| !name.starts_with('v'))
    {
        return Err(invalid(
            "managed runtime is not in an immutable version directory",
        ));
    }
    // SAFETY: geteuid has no preconditions and reads this process's effective user ID.
    let uid = unsafe { libc::geteuid() };
    for path in [&managed_root, &versions_directory, &version_directory] {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir()
            || fs::canonicalize(path)? != *path
            || metadata.uid() != uid
            || metadata.mode() & 0o022 != 0
        {
            return Err(invalid(
                "managed runtime directory is not a private real directory",
            ));
        }
    }
    for path in [&router, &engine] {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.nlink() != 1
            || metadata.mode() & 0o022 != 0
        {
            return Err(invalid(
                "managed runtime binary is not a protected unique file",
            ));
        }
    }

    let router_sha256 = binary_digest(&router)?;
    let engine_sha256 = binary_digest(&engine)?;
    let router_identity = version_identity(&router)?;
    let engine_identity = version_identity(&engine)?;
    if router_identity != engine_identity {
        return Err(invalid("managed router and engine build identities differ"));
    }
    // Re-hash after the bounded version probes so the recorded pair was stable
    // across both metadata and executable observations.
    if binary_digest(&router)? != router_sha256 || binary_digest(&engine)? != engine_sha256 {
        return Err(invalid("managed runtime pair changed during verification"));
    }
    let runtime_version = router_identity
        .split_whitespace()
        .next()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("managed runtime has no source version identity"))?
        .to_string();
    Ok(RuntimePair {
        managed_root,
        version_directory,
        runtime_version,
        runtime_build_identity: router_identity,
        router,
        engine,
        router_sha256,
        engine_sha256,
    })
}

fn version_identity(binary: &Path) -> io::Result<String> {
    use crate::bounded_output::{LimitedOutput, output_limited};
    use std::process::Command;

    let output = output_limited(
        Command::new(binary).arg("--version"),
        VERSION_PROBE_BUDGET,
        VERSION_PROBE_BYTES,
    )?;
    let LimitedOutput::Completed(output) = output else {
        return Err(invalid("managed runtime version probe exceeded its bound"));
    };
    if !output.status.success() {
        return Err(invalid("managed runtime version probe failed"));
    }
    let banner = String::from_utf8(output.stdout).map_err(io::Error::other)?;
    let first = banner
        .lines()
        .next()
        .ok_or_else(|| invalid("managed runtime returned an empty version banner"))?;
    let identity = crate::install_health::build_identity(first)
        .ok_or_else(|| invalid("managed runtime version banner has no build identity"))?;
    Ok(identity.to_string())
}

fn current_repository() -> io::Result<(PathBuf, String)> {
    let cwd = std::env::current_dir()?;
    let repo = crate::GitRepo::discover(&cwd).map_err(io::Error::other)?;
    let repository = repo.main_root().map_err(io::Error::other)?;
    let common = repo.git_common_dir().map_err(io::Error::other)?;
    let uuid = crate::store::repository_identity::read_repository_uuid_marker(&common)
        .map_err(io::Error::other)?
        .ok_or_else(|| invalid("repository has no initialized Aethyme UUID marker"))?;
    Ok((fs::canonicalize(repository)?, uuid))
}

pub(crate) fn service_plan() -> io::Result<Plan> {
    let (repository, uuid) = current_repository()?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| invalid("HOME is required for a per-user GC service"))?;
    let home = absolute_directory(&home)?;
    let current = std::env::current_exe()?;
    let runtime = runtime_pair(&current)?;
    Plan::from_runtime(
        &repository,
        &uuid,
        &home,
        // SAFETY: geteuid has no preconditions and reads this process's effective user ID.
        unsafe { libc::geteuid() },
        &runtime,
    )
}

fn expected_directory(home: &Path, uuid: &str) -> PathBuf {
    home.join(SERVICE_DIRECTORY).join(uuid)
}

fn valid_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_plan(plan: &Plan, bytes: &[u8], home: &Path, uuid: &str, uid: u32) -> io::Result<()> {
    crate::store::repository_identity::validate_uuid_v4(uuid).map_err(io::Error::other)?;
    let directory = expected_directory(home, uuid);
    let versions_directory = plan.managed_root.join("versions");
    let label = format!("com.aethyme.gc-sweep.{uuid}");
    if plan.schema != PLAN_SCHEMA
        || plan.repository_uuid != uuid
        || plan.home != home
        || plan.uid != uid
        || plan.directory != directory
        || plan.receipt_path != directory.join("last-pass.json")
        || plan.label != label
        || plan.unit_path != home.join(UNIT_DIRECTORY).join(format!("{label}.plist"))
        || plan.interval_seconds != SERVICE_INTERVAL_SECONDS
        || !valid_hex_digest(&plan.generation)
        || !valid_hex_digest(&plan.digest)
        || !valid_hex_digest(&plan.router_sha256)
        || !valid_hex_digest(&plan.engine_sha256)
        || plan.router.file_name().and_then(|name| name.to_str()) != Some("aethyme")
        || plan.engine.file_name().and_then(|name| name.to_str()) != Some("aethyme-engine-cli")
        || plan.router.parent() != Some(plan.version_directory.as_path())
        || plan.engine.parent() != Some(plan.version_directory.as_path())
        || plan.version_directory.parent() != Some(versions_directory.as_path())
        || plan.runtime_build_identity.split_whitespace().next()
            != Some(plan.runtime_version.as_str())
    {
        return Err(invalid(
            "installed GC service plan does not match its repository namespace",
        ));
    }
    if !plan.repository.is_absolute()
        || plan.repository.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(invalid(
            "installed GC service repository path is not canonical",
        ));
    }
    if generation(plan)? != plan.generation || render_unit(plan)? != plan.unit {
        return Err(invalid("installed GC service generation or unit changed"));
    }
    let mut unsigned = plan.clone();
    unsigned.digest.clear();
    if digest(&serde_json::to_vec(&unsigned).map_err(io::Error::other)?) != plan.digest
        || serde_json::to_vec(plan).map_err(io::Error::other)? != bytes
    {
        return Err(invalid(
            "installed GC service plan digest or encoding changed",
        ));
    }
    Ok(())
}

fn load(home: &Path, uuid: &str) -> io::Result<(Plan, Vec<u8>)> {
    let home = absolute_directory(home)?;
    // SAFETY: geteuid has no preconditions and reads this process's effective user ID.
    let uid = unsafe { libc::geteuid() };
    if uid == 0 {
        return Err(invalid("GC sweep service requires a non-root user"));
    }
    let directory = expected_directory(&home, uuid);
    crate::pr_watch_service_install::lifecycle::check_directories(&home, &directory, uid, true)?;
    let bytes = crate::pr_watch_service_install::lifecycle::checked_file(
        &directory.join("plan.json"),
        uid,
    )?;
    let plan: Plan = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    validate_plan(&plan, &bytes, &home, uuid, uid)?;
    crate::pr_watch_service_install::lifecycle::check_directories(
        &home,
        &home.join(UNIT_DIRECTORY),
        uid,
        false,
    )?;
    let unit = crate::pr_watch_service_install::lifecycle::checked_file(&plan.unit_path, uid)?;
    if unit != plan.unit.as_bytes() {
        return Err(invalid("installed GC service unit changed"));
    }
    Ok((plan, bytes))
}

fn verify_runtime(plan: &Plan) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    // SAFETY: geteuid has no preconditions and reads this process's effective user ID.
    let uid = unsafe { libc::geteuid() };
    let runtime = RuntimePair {
        managed_root: plan.managed_root.clone(),
        version_directory: plan.version_directory.clone(),
        runtime_version: plan.runtime_version.clone(),
        runtime_build_identity: plan.runtime_build_identity.clone(),
        router: plan.router.clone(),
        engine: plan.engine.clone(),
        router_sha256: plan.router_sha256.clone(),
        engine_sha256: plan.engine_sha256.clone(),
    };
    let fresh = runtime_pair(&plan.router)?;
    if fresh != runtime {
        return Err(invalid(
            "installed GC service runtime pair is stale or changed",
        ));
    }
    if fs::canonicalize(&plan.router)? != plan.router
        || fs::canonicalize(&plan.engine)? != plan.engine
    {
        return Err(invalid("installed GC runtime path is no longer canonical"));
    }
    for path in [&plan.router, &plan.engine] {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.uid() != uid || metadata.nlink() != 1 {
            return Err(invalid(
                "installed GC runtime binary identity is not protected",
            ));
        }
    }
    Ok(())
}

pub(crate) fn install(plan: &Plan, confirmation: &str) -> io::Result<Vec<PathBuf>> {
    if confirmation != plan.digest {
        return Err(invalid("GC service install requires the exact plan digest"));
    }
    let fresh = service_plan()?;
    let installed_bytes = serde_json::to_vec(plan).map_err(io::Error::other)?;
    if !matches_fresh_generation(&fresh, &installed_bytes)? {
        return Err(invalid(
            "GC service plan changed; recompute and review it before installation",
        ));
    }
    #[cfg(target_os = "macos")]
    let state = crate::scheduled_service::launchd::label_state;
    #[cfg(not(target_os = "macos"))]
    let state = |_: u32, _: &str| {
        Ok::<_, io::Error>(crate::scheduled_service::launchd::LabelState::Unknown)
    };
    install_replacing_dead_record(plan, state).map(|(created, _)| created)
}

#[cfg(test)]
fn install_exact_plan_files(plan: &Plan) -> io::Result<Vec<PathBuf>> {
    let bytes = serde_json::to_vec(plan).map_err(io::Error::other)?;
    let files = [("plan.json", bytes.as_slice())];
    crate::pr_watch_service_install::install_exact_files(
        &plan.home,
        &plan.directory,
        &plan.unit_path,
        plan.uid,
        &files,
        plan.unit.as_bytes(),
    )
}

fn matches_fresh_generation(fresh: &Plan, installed_bytes: &[u8]) -> io::Result<bool> {
    Ok(serde_json::to_vec(fresh).map_err(io::Error::other)? == installed_bytes)
}

fn now_unix_ms() -> io::Result<u64> {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis();
    Ok(u64::try_from(value).unwrap_or(u64::MAX))
}

fn read_receipt_bytes(path: &Path, uid: u32) -> io::Result<Option<Vec<u8>>> {
    use std::os::unix::fs::MetadataExt as _;
    let Some((bytes, metadata)) = crate::pr_watch_service_install::existing(path, uid)? else {
        return Ok(None);
    };
    if metadata.nlink() != 1 || bytes.len() > MAX_RECEIPT_BYTES {
        return Err(invalid(
            "GC service receipt is linked or exceeds its read budget",
        ));
    }
    Ok(Some(bytes))
}

fn read_control(plan: &Plan) -> io::Result<Option<ControlReceipt>> {
    let path = plan.directory.join("control.json");
    let uid = plan.uid;
    let Some(bytes) = read_receipt_bytes(&path, uid)? else {
        return Ok(None);
    };
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(invalid(
            "GC service control receipt exceeds its read budget",
        ));
    }
    let receipt: ControlReceipt = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if receipt.schema != CONTROL_SCHEMA
        || receipt.plan_digest != plan.digest
        || receipt.generation != plan.generation
    {
        return Err(invalid(
            "GC service control receipt belongs to another generation",
        ));
    }
    Ok(Some(receipt))
}

fn write_control(plan: &Plan, receipt: &ControlReceipt) -> io::Result<()> {
    let bytes = serde_json::to_vec(receipt).map_err(io::Error::other)?;
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(invalid(
            "GC service control receipt exceeds its write budget",
        ));
    }
    let target = plan.directory.join("control.json");
    let _previous_control = read_control(plan)?;
    crate::atomic_file::with_synced_temporary(&target, &bytes, |temporary| {
        fs::rename(temporary, &target)
    })?;
    fs::File::open(&plan.directory)?.sync_all()
}

fn read_last_pass(plan: &Plan) -> io::Result<Option<LastPassReceipt>> {
    let uid = plan.uid;
    let Some(bytes) = read_receipt_bytes(&plan.receipt_path, uid)? else {
        return Ok(None);
    };
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(invalid(
            "GC service last-pass receipt exceeds its read budget",
        ));
    }
    let receipt: LastPassReceipt = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if receipt.schema != RECEIPT_SCHEMA
        || receipt.repository_uuid != plan.repository_uuid
        || receipt.generation != plan.generation
    {
        return Err(invalid(
            "GC service last-pass receipt belongs to another generation",
        ));
    }
    Ok(Some(receipt))
}

fn bounded_error(error: &str) -> String {
    error
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect()
}

fn write_last_pass(
    plan: &Plan,
    started_at_unix_ms: u64,
    report: Option<&crate::ArtifactSweepReport>,
    error: Option<&str>,
) -> io::Result<()> {
    // Parse and bind any previous receipt before replacing it.
    let previous_pass = read_last_pass(plan)?;
    let finished_at_unix_ms = now_unix_ms()?;
    let previous_trash = previous_pass.as_ref().and_then(|previous| previous.trash);
    let drain = report
        .and_then(|report| trash_drain_since(previous_trash.as_ref(), &report.checkout_trash));
    // A failed pass neither extends nor resets the run of empty passes.
    let reclaimed = report.map(|report| pass_reclaimed(report, drain));
    let (passes_without_reclaim, unreclaimed_since_unix_ms) = match (reclaimed, &previous_pass) {
        (Some(true), _) => (0, None),
        (Some(false), Some(previous)) if previous.passes_without_reclaim > 0 => (
            previous.passes_without_reclaim.saturating_add(1),
            previous
                .unreclaimed_since_unix_ms
                .or(Some(started_at_unix_ms)),
        ),
        (Some(false), _) => (1, Some(started_at_unix_ms)),
        (None, Some(previous)) => (
            previous.passes_without_reclaim,
            previous.unreclaimed_since_unix_ms,
        ),
        (None, None) => (0, None),
    };
    let receipt = LastPassReceipt {
        schema: RECEIPT_SCHEMA.to_string(),
        repository_uuid: plan.repository_uuid.clone(),
        generation: plan.generation.clone(),
        started_at_unix_ms,
        finished_at_unix_ms,
        outcome: match (report, error) {
            (_, Some(_)) => "error".to_string(),
            (Some(report), None) if report.complete => "sweep_complete".to_string(),
            (Some(_), None) => "sweep_paused".to_string(),
            _ => "unknown".to_string(),
        },
        directories_reclaimed: report.map(|report| report.directories_reclaimed),
        budget_ms: report.map(|report| report.budget_ms),
        disk_pressured: report.map(|report| report.disk_pressured),
        legacy_unscoped_state: report.map(|report| match report.legacy_unscoped_state {
            crate::ArtifactSweepLegacyUnscopedState::Clear => "clear".to_string(),
            crate::ArtifactSweepLegacyUnscopedState::UnknownPending => {
                "unknown_pending".to_string()
            }
        }),
        error: error.map(bounded_error),
        artifact_lane: report.map(|report| report.artifact_lane.as_str().to_string()),
        entries_reclaimed: report.map(|report| report.entries_reclaimed),
        bytes_reclaimed: report.map(|report| {
            report
                .observed_logical_file_bytes
                .saturating_add(report.preparation_cache_bytes_reclaimed)
        }),
        checkouts_removed: report
            .and_then(|report| report.auto_cleanup.as_ref())
            .map(|cleanup| cleanup.removed.len()),
        checkouts_kept: report
            .and_then(|report| report.auto_cleanup.as_ref())
            .map(|cleanup| cleanup.kept.len()),
        // A failed pass keeps the last observation, so the next pass still
        // measures the deleter against it.
        trash: report
            .map(|report| report.checkout_trash)
            .or(previous_trash),
        trash_entries_drained: drain.map(|drain| drain.entries),
        trash_entries_held_back: drain.map(|drain| drain.held_back),
        reclaimed_nothing_because: report
            .filter(|report| !pass_reclaimed(report, drain))
            .and_then(|report| reclaimed_nothing_because(report, drain))
            .map(str::to_string),
        passes_without_reclaim,
        unreclaimed_since_unix_ms,
    };
    let bytes = serde_json::to_vec(&receipt).map_err(io::Error::other)?;
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(invalid(
            "GC service last-pass receipt exceeds its write budget",
        ));
    }
    crate::atomic_file::with_synced_temporary(&plan.receipt_path, &bytes, |temporary| {
        fs::rename(temporary, &plan.receipt_path)
    })?;
    fs::File::open(&plan.directory)?.sync_all()
}

#[cfg(target_os = "macos")]
pub(crate) fn managed_tick_requested() -> bool {
    std::env::var_os(SERVICE_ENV_UUID).is_some()
        || std::env::var_os(SERVICE_ENV_GENERATION).is_some()
}

#[cfg(target_os = "macos")]
pub(crate) fn preflight_managed_tick(snapshot: &crate::Broker) -> io::Result<Option<ManagedTick>> {
    if !managed_tick_requested() {
        return Ok(None);
    }
    let uuid = std::env::var(SERVICE_ENV_UUID)
        .map_err(|_| invalid("managed GC tick is missing its repository UUID"))?;
    let generation = std::env::var(SERVICE_ENV_GENERATION)
        .map_err(|_| invalid("managed GC tick is missing its service generation"))?;
    if !valid_hex_digest(&generation) {
        return Err(invalid("managed GC tick generation marker is invalid"));
    }
    let repository = fs::canonicalize(snapshot.main_root_path())?;
    let common = snapshot
        .repo_handle()
        .git_common_dir()
        .map_err(io::Error::other)?;
    let actual_uuid = crate::store::repository_identity::read_repository_uuid_marker(&common)
        .map_err(io::Error::other)?
        .ok_or_else(|| invalid("managed GC repository UUID marker is missing"))?;
    if actual_uuid != uuid {
        return Err(invalid(
            "managed GC tick repository UUID does not match this checkout",
        ));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| invalid("managed GC tick has no HOME"))?;
    let home = absolute_directory(&home)?;
    let (plan, bytes) = load(&home, &uuid)?;
    if plan.repository != repository || plan.generation != generation {
        return Err(invalid(
            "managed GC tick repository or generation does not match its plan",
        ));
    }
    let current = executable(&std::env::current_exe()?)?;
    if current != plan.router {
        return Err(invalid(
            "managed GC tick is not running from the pinned router binary",
        ));
    }
    verify_runtime(&plan)?;
    let _claim = crate::pr_watch_service_install::claim_installer(&plan.directory, plan.uid)?;
    if load(&home, &uuid)?.1 != bytes {
        return Err(invalid("managed GC service plan changed during preflight"));
    }
    verify_runtime(&plan)?;
    Ok(Some(ManagedTick {
        plan,
        started_at_unix_ms: now_unix_ms()?,
        _claim,
    }))
}

#[cfg(target_os = "macos")]
pub(crate) fn record_managed_tick(
    tick: &ManagedTick,
    report: Option<&crate::ArtifactSweepReport>,
    error: Option<&str>,
) -> io::Result<()> {
    write_last_pass(&tick.plan, tick.started_at_unix_ms, report, error)
}

#[cfg(target_os = "macos")]
fn launchctl(args: &[String]) -> io::Result<crate::scheduled_service::launchd::Observation> {
    crate::scheduled_service::launchd::launchctl(args)
}

#[cfg(target_os = "macos")]
pub(crate) fn control(
    home: &Path,
    uuid: &str,
    action: &str,
    confirmation: Option<&str>,
) -> io::Result<Value> {
    if action == "status"
        && let Ok((repository, marker_uuid)) = current_repository()
        && let Ok(running) = std::env::current_exe()
        && automatic_refresh_proof(home, uuid, &marker_uuid, &repository, &running).is_ok()
    {
        let refreshed = refresh_current(home, uuid);
        // A refresh that stopped part-way leaves no loadable plan: report
        // the refresh itself rather than failing the status call.
        let mut status =
            control_with(home, uuid, "status", None, launchctl).unwrap_or_else(|error| {
                json!({
                    "schema": CONTROL_SCHEMA,
                    "action": "status",
                    "installation_state": "unreadable_after_refresh",
                    "error": bounded_error(&error.to_string()),
                    "next_action": REFRESH_COMMAND,
                    "delivery_proven": false
                })
            });
        if let Some(object) = status.as_object_mut() {
            object.insert(
                "automatic_refresh".into(),
                serde_json::to_value(&refreshed).map_err(io::Error::other)?,
            );
        }
        return Ok(status);
    }
    if action == "status"
        && let Ok((repository, marker_uuid)) = current_repository()
    {
        let retry = retry_unconfirmed_load_with(
            home,
            uuid,
            &marker_uuid,
            &repository,
            crate::scheduled_service::launchd::label_state,
            launchctl,
        )
        .unwrap_or_else(|error| {
            Some(json!({"retried": false, "error": bounded_error(&error.to_string())}))
        });
        if let Some(retry) = retry {
            let mut status = control_with(home, uuid, "status", None, launchctl)?;
            if let Some(object) = status.as_object_mut() {
                object.insert("load_retry".into(), retry);
            }
            return Ok(status);
        }
    }
    if matches!(action, "status" | "uninstall")
        && let Some(result) = control_dead_record(
            home,
            uuid,
            action,
            confirmation,
            crate::scheduled_service::launchd::label_state,
        )?
    {
        return Ok(result);
    }
    control_with(home, uuid, action, confirmation, launchctl)
}

fn control_with(
    home: &Path,
    uuid: &str,
    action: &str,
    confirmation: Option<&str>,
    mut run: impl FnMut(&[String]) -> io::Result<crate::scheduled_service::launchd::Observation>,
) -> io::Result<Value> {
    use crate::scheduled_service::launchd::Observation;

    if !matches!(action, "status" | "activate" | "deactivate" | "uninstall") {
        return Err(invalid("unknown GC service control"));
    }
    let (plan, bytes) = load(home, uuid)?;
    if action != "status" && confirmation != Some(plan.digest.as_str()) {
        return Err(invalid(
            "GC service state change requires the exact installed plan digest",
        ));
    }
    if action == "uninstall" {
        return uninstall_loaded(&plan, &bytes);
    }
    if action == "status" {
        let observation = run(&["print".into(), format!("gui/{}/{}", plan.uid, plan.label)])?;
        let runtime_state = verify_runtime(&plan)
            .map(|()| "verified")
            .unwrap_or("stale_or_unavailable");
        let control = read_control(&plan)?;
        let last_pass = read_last_pass(&plan)?;
        let last_error = read_bounded_json(&plan.directory.join("last-error.json"), plan.uid);
        let last_refresh = read_bounded_json(&plan.directory.join("last-refresh.json"), plan.uid);
        let binding = runtime_binding(&plan.router, &plan.managed_root);
        let (last_error_at, last_pass_at) = tick_timestamps(&plan.directory, plan.uid);
        // Until the service runs a verified current pair again, say how.
        let next_action = (runtime_state != "verified"
            || binding.needs_refresh()
            || tick_refusing(last_error_at, last_pass_at))
        .then_some(REFRESH_COMMAND);
        return Ok(json!({
            "schema": CONTROL_SCHEMA,
            "action": "status",
            "plan_digest": plan.digest,
            "generation": plan.generation,
            "scheduler_state": if observation == Observation::Success { "loaded" } else { "unknown" },
            "scheduler_observation": observation_name(&observation),
            "runtime_state": runtime_state,
            "runtime_binding": binding.as_str(),
            "control": control,
            "last_pass": last_pass,
            "last_error": last_error,
            "last_refresh": last_refresh,
            "next_action": next_action,
            "delivery_proven": false
        }));
    }

    let _claim = crate::pr_watch_service_install::claim_installer(&plan.directory, plan.uid)?;
    if load(home, uuid)?.1 != bytes {
        return Err(invalid(
            "GC service installation changed while claiming control",
        ));
    }
    if action == "activate" {
        let fresh = service_plan()?;
        if !matches_fresh_generation(&fresh, &bytes)? {
            return Err(invalid(
                "GC runtime generation is obsolete; deactivate and uninstall, then review a new plan",
            ));
        }
        verify_runtime(&plan)?;
    }
    let attempt = ControlReceipt {
        schema: CONTROL_SCHEMA.to_string(),
        plan_digest: plan.digest.clone(),
        generation: plan.generation.clone(),
        action: action.to_string(),
        scheduler_state: "unknown".to_string(),
        operation_result: "attempting".to_string(),
        observed_at_unix_ms: now_unix_ms()?,
    };
    write_control(&plan, &attempt)?;
    let target = format!("gui/{}/{}", plan.uid, plan.label);
    let args = if action == "activate" {
        vec![
            "bootstrap".to_string(),
            format!("gui/{}", plan.uid),
            plan.unit_path.to_string_lossy().into_owned(),
        ]
    } else {
        vec!["bootout".to_string(), target]
    };
    let observation = run(&args)?;
    let (scheduler_state, operation_result) = match (action, observation) {
        ("activate", Observation::Success) => ("loaded", "acknowledged"),
        ("deactivate", Observation::Success) => ("unloaded", "acknowledged"),
        (_, Observation::Refused) => ("unknown", "refused"),
        (_, Observation::Unknown) => ("unknown", "unknown"),
        _ => ("unknown", "unknown"),
    };
    let receipt = ControlReceipt {
        schema: CONTROL_SCHEMA.to_string(),
        plan_digest: plan.digest.clone(),
        generation: plan.generation.clone(),
        action: action.to_string(),
        scheduler_state: scheduler_state.to_string(),
        operation_result: operation_result.to_string(),
        observed_at_unix_ms: now_unix_ms()?,
    };
    write_control(&plan, &receipt)?;
    Ok(json!({
        "schema": CONTROL_SCHEMA,
        "action": action,
        "plan_digest": plan.digest,
        "generation": plan.generation,
        "scheduler_state": scheduler_state,
        "operation_result": operation_result,
        "delivery_proven": false
    }))
}

fn observation_name(observation: &crate::scheduled_service::launchd::Observation) -> &'static str {
    use crate::scheduled_service::launchd::Observation;
    match observation {
        Observation::Success => "success",
        Observation::Refused => "refused",
        Observation::Unknown => "unknown",
    }
}

fn uninstall_loaded(plan: &Plan, bytes: &[u8]) -> io::Result<Value> {
    let _claim = crate::pr_watch_service_install::claim_installer(&plan.directory, plan.uid)?;
    if load(&plan.home, &plan.repository_uuid)?.1 != bytes {
        return Err(invalid(
            "GC service installation changed while claiming uninstall",
        ));
    }
    let control = read_control(plan)?.ok_or_else(|| {
        invalid("GC service uninstall requires a successful deactivate operation first")
    })?;
    if control.action != "deactivate"
        || control.scheduler_state != "unloaded"
        || control.operation_result != "acknowledged"
    {
        return Err(invalid(
            "GC service uninstall requires proof of successful deactivation",
        ));
    }
    let _previous_pass = read_last_pass(plan)?;
    let _previous_control = read_control(plan)?;
    let files = [
        plan.unit_path.clone(),
        plan.directory.join("plan.json"),
        plan.receipt_path.clone(),
        plan.directory.join("control.json"),
    ];
    for path in &files {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => fs::remove_file(path)?,
            Ok(_) => return Err(invalid("GC service uninstall found a non-file owned path")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
    }
    Ok(json!({
        "schema": CONTROL_SCHEMA,
        "action": "uninstall",
        "plan_digest": plan.digest,
        "generation": plan.generation,
        "scheduler_state": "unloaded",
        "installed_files_removed": true,
        "delivery_proven": false
    }))
}

/// Archived installation records kept beside a service, newest first.
const RECORD_ARCHIVES_KEPT: usize = 5;
/// Where replaced or removed installation records go, inside the service
/// directory (never under `LaunchAgents`, where launchd would load a unit).
const RECORD_ARCHIVE_DIRECTORY: &str = "archive";

/// What is recorded for this repository's service, read without requiring
/// the unit file or a valid current plan: enough to tell whether the record
/// is dead. Every field comes from the repository's own namespace.
#[derive(Debug, Clone)]
pub(crate) struct InstallationRecord {
    pub(crate) directory: PathBuf,
    pub(crate) unit_path: PathBuf,
    pub(crate) label: String,
    pub(crate) uid: u32,
    pub(crate) plan_bytes: Option<Vec<u8>>,
    pub(crate) plan_digest: Option<String>,
    pub(crate) runtime_version: Option<String>,
    pub(crate) router: Option<PathBuf>,
    /// The recorded installation root, whose `current` selector says
    /// whether the recorded runtime is still the selected one.
    pub(crate) managed_root: Option<PathBuf>,
    pub(crate) unit_present: bool,
    /// SHA-256 of the unit file when it is a readable private owned file:
    /// what `uninstall` takes as confirmation when no plan is recorded.
    pub(crate) unit_digest: Option<String>,
}

impl InstallationRecord {
    /// Whether anything is recorded at all.
    fn exists(&self) -> bool {
        self.plan_bytes.is_some() || self.unit_present
    }

    fn runtime_present(&self) -> Option<bool> {
        self.router.as_ref().map(|router| router.exists())
    }

    /// Why this record can be replaced or removed without touching a live
    /// service: launchd must answer that the label is not loaded (an unknown
    /// answer proves nothing), and either the unit file is gone or the
    /// recorded runtime binary it would run is gone.
    pub(crate) fn dead_reason(
        &self,
        label: crate::scheduled_service::launchd::LabelState,
    ) -> Option<&'static str> {
        if label != crate::scheduled_service::launchd::LabelState::NotLoaded {
            return None;
        }
        if !self.unit_present {
            return Some("unit_file_missing_and_label_not_loaded");
        }
        if self.plan_bytes.is_none() {
            return Some("plan_missing_and_label_not_loaded");
        }
        if self.runtime_present() == Some(false) {
            return Some("recorded_runtime_missing_and_label_not_loaded");
        }
        None
    }

    /// The digest `uninstall` must be confirmed with: the recorded plan's,
    /// or the unit file's when no plan is recorded.
    fn confirmation_digest(&self) -> Option<&str> {
        if self.plan_bytes.is_some() {
            self.plan_digest.as_deref()
        } else {
            self.unit_digest.as_deref()
        }
    }

    fn describe(&self, label: crate::scheduled_service::launchd::LabelState) -> Value {
        json!({
            "recorded_plan_digest": self.plan_digest,
            "recorded_runtime_version": self.runtime_version,
            "recorded_runtime_present": self.runtime_present(),
            "plan_file": if self.plan_bytes.is_some() { "present" } else { "missing" },
            "unit_file": if self.unit_present { "present" } else { "missing" },
            "recorded_unit_digest": self.unit_digest,
            "scheduler_state": label_state_name(label),
            "replaceable": self.dead_reason(label).is_some(),
            "replaceable_because": self.dead_reason(label),
        })
    }
}

fn label_state_name(label: crate::scheduled_service::launchd::LabelState) -> &'static str {
    use crate::scheduled_service::launchd::LabelState;
    match label {
        LabelState::Loaded => "loaded",
        LabelState::NotLoaded => "not_loaded",
        LabelState::Unknown => "unknown",
    }
}

/// Read what is recorded under this repository's service namespace.
pub(crate) fn read_record(home: &Path, uuid: &str, uid: u32) -> io::Result<InstallationRecord> {
    let directory = expected_directory(home, uuid);
    let label = format!("com.aethyme.gc-sweep.{uuid}");
    let unit_path = home.join(UNIT_DIRECTORY).join(format!("{label}.plist"));
    let plan_bytes = match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() => {
            crate::pr_watch_service_install::existing(&directory.join("plan.json"), uid)?
                .map(|(bytes, _)| bytes)
        }
        Ok(_) => return Err(invalid("GC service directory is not a directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let recorded: Option<Value> = plan_bytes
        .as_deref()
        .and_then(|bytes| serde_json::from_slice(bytes).ok());
    let field = |name: &str| {
        recorded
            .as_ref()
            .and_then(|value| value.get(name))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let unit_present = match fs::symlink_metadata(&unit_path) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    let unit_digest = if unit_present {
        crate::pr_watch_service_install::existing(&unit_path, uid)
            .ok()
            .flatten()
            .map(|(bytes, _)| digest(&bytes))
    } else {
        None
    };
    Ok(InstallationRecord {
        directory,
        unit_path,
        label,
        uid,
        plan_digest: field("digest"),
        runtime_version: field("runtime_version"),
        router: field("router").map(PathBuf::from),
        managed_root: field("managed_root").map(PathBuf::from),
        plan_bytes,
        unit_present,
        unit_digest,
    })
}

/// Move a dead record's files (its unit when present, then its plan and
/// receipts) into `archive/<ms>/` inside the service directory, keeping the
/// newest [`RECORD_ARCHIVES_KEPT`] archives. Moves only; nothing of the
/// record is deleted here. The caller holds the directory's installer claim.
///
/// The unit goes first: a crash part-way leaves a plan without its unit,
/// which `status`, `install` and `uninstall` already treat as a stale record,
/// rather than a unit without its plan.
fn archive_record(record: &InstallationRecord) -> io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt as _;
    let _span = crate::phase_telemetry::span("gc_service_record_archive");
    let archives = record.directory.join(RECORD_ARCHIVE_DIRECTORY);
    match fs::DirBuilder::new().mode(0o700).create(&archives) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    if !fs::symlink_metadata(&archives)?.is_dir() {
        return Err(invalid("GC service archive path is not a directory"));
    }
    if record.unit_present && !fs::symlink_metadata(&record.unit_path)?.is_file() {
        return Err(invalid("GC service unit path is not a file"));
    }
    let mut stamp = now_unix_ms()?;
    let target = loop {
        let candidate = archives.join(stamp.to_string());
        match fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => stamp += 1,
            Err(error) => return Err(error),
        }
    };
    if record.unit_present {
        move_file(&record.unit_path, &target.join("unit.plist"))?;
        if let Some(parent) = record.unit_path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
    }
    for name in [
        "plan.json",
        "control.json",
        "last-pass.json",
        "last-error.json",
    ] {
        let source = record.directory.join(name);
        match fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.is_file() => {
                fs::rename(&source, target.join(name))?;
            }
            Ok(_) => return Err(invalid("GC service record holds a non-file path")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    fs::File::open(&target)?.sync_all()?;
    fs::File::open(&record.directory)?.sync_all()?;
    prune_archives(&archives);
    crate::phase_telemetry::add("gc_service_record_archived", Duration::ZERO);
    Ok(target)
}

/// Keep the newest [`RECORD_ARCHIVES_KEPT`] archives; only numeric names are
/// archives, anything else is left alone.
fn prune_archives(archives: &Path) {
    let Ok(entries) = fs::read_dir(archives) else {
        return;
    };
    let mut kept: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let stamp = entry.file_name().to_str()?.parse::<u64>().ok()?;
            Some((stamp, entry.path()))
        })
        .collect();
    kept.sort_by_key(|(stamp, _)| std::cmp::Reverse(*stamp));
    for (_, old) in kept.into_iter().skip(RECORD_ARCHIVES_KEPT) {
        crate::warn_unrecorded(
            "prune an old GC service record archive",
            fs::remove_dir_all(old),
        );
    }
}

/// Move one owned file. `LaunchAgents` and the service directory can sit on
/// different volumes, where a rename fails with `EXDEV`: then copy, sync and
/// only then remove the source.
fn move_file(source: &Path, destination: &Path) -> io::Result<()> {
    move_file_with(source, destination, |from, to| fs::rename(from, to))
}

fn move_file_with(
    source: &Path,
    destination: &Path,
    rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    match rename(source, destination) {
        Ok(()) => return Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {}
        Err(error) => return Err(error),
    }
    let mut from = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(source)?;
    if !from.metadata()?.is_file() {
        return Err(invalid("GC service record holds a non-file path"));
    }
    let mut to = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)?;
    io::copy(&mut from, &mut to)?;
    to.sync_all()?;
    if let Some(parent) = destination.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    fs::remove_file(source)
}

/// Install `plan`, first archiving a different recorded installation when
/// it is provably dead; a live or undecidable one keeps the refusal.
fn install_replacing_dead_record(
    plan: &Plan,
    mut label_state: impl FnMut(u32, &str) -> io::Result<crate::scheduled_service::launchd::LabelState>,
) -> io::Result<(Vec<PathBuf>, Option<PathBuf>)> {
    let new_bytes = serde_json::to_vec(plan).map_err(io::Error::other)?;
    // Decide under the installer claim, so a concurrent install cannot
    // judge this record dead and archive what this one just published.
    let claim = crate::pr_watch_service_install::claim_service_directory(
        &plan.home,
        &plan.directory,
        plan.uid,
    )?;
    let record = read_record(&plan.home, &plan.repository_uuid, plan.uid)?;
    let differs = record
        .plan_bytes
        .as_deref()
        .is_some_and(|bytes| bytes != new_bytes)
        || (record.unit_present
            && fs::read(&record.unit_path).ok().as_deref() != Some(plan.unit.as_bytes()));
    let archived = if record.exists() && differs {
        let state = label_state(record.uid, &record.label)?;
        match record.dead_reason(state) {
            Some(_) => Some(archive_record(&record)?),
            None => {
                return Err(invalid(match state {
                    crate::scheduled_service::launchd::LabelState::Loaded => {
                        "a different GC service is loaded; deactivate and uninstall it before installing a new plan"
                    }
                    crate::scheduled_service::launchd::LabelState::Unknown => {
                        "a different GC service installation is recorded and launchd could not say whether it is loaded; refusing to replace it"
                    }
                    crate::scheduled_service::launchd::LabelState::NotLoaded => {
                        "a different GC service installation is recorded with its unit and runtime in place; uninstall it before installing a new plan"
                    }
                }));
            }
        }
    } else {
        None
    };
    let bytes = serde_json::to_vec(plan).map_err(io::Error::other)?;
    let created = crate::pr_watch_service_install::install_exact_files_claimed(
        &plan.home,
        &plan.directory,
        &plan.unit_path,
        plan.uid,
        &[("plan.json", bytes.as_slice())],
        plan.unit.as_bytes(),
        &claim,
    )?;
    Ok((created, archived))
}

/// `service status` and `uninstall` for a record whose unit file is gone:
/// report it, or finish removing it when it is provably dead.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn control_dead_record(
    home: &Path,
    uuid: &str,
    action: &str,
    confirmation: Option<&str>,
    mut label_state: impl FnMut(u32, &str) -> io::Result<crate::scheduled_service::launchd::LabelState>,
) -> io::Result<Option<Value>> {
    // SAFETY: geteuid has no preconditions and reads this process's effective user ID.
    let uid = unsafe { libc::geteuid() };
    let record = read_record(home, uuid, uid)?;
    if !record.exists() {
        // A refresh that could not restore the record leaves it archived.
        return Ok(
            (action == "status")
                .then(|| interrupted_refresh_archive(&record.directory, uid))
                .flatten()
                .map(|archive| {
                    json!({
                        "schema": CONTROL_SCHEMA,
                        "action": "status",
                        "installation_state": "refresh_interrupted",
                        "archived_record": archive,
                        "last_refresh": read_bounded_json(&record.directory.join("last-refresh.json"), uid),
                        "next_action": REFRESH_COMMAND,
                        "delivery_proven": false
                    })
                }),
        );
    }
    if record.unit_present && record.plan_bytes.is_some() {
        return Ok(None);
    }
    if action == "uninstall" {
        return uninstall_dead_record(home, uuid, uid, confirmation, label_state).map(Some);
    }
    let state = label_state(uid, &record.label)?;
    match action {
        "status" => Ok(Some(json!({
            "schema": CONTROL_SCHEMA,
            "action": "status",
            "installation_state": "stale_record",
            "plan_digest": record.plan_digest,
            "record": record.describe(state),
            "delivery_proven": false
        }))),
        _ => Ok(None),
    }
}

/// Finish removing a record that has its plan or its unit but not both,
/// deciding under the installer claim on a record read under it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn uninstall_dead_record(
    home: &Path,
    uuid: &str,
    uid: u32,
    confirmation: Option<&str>,
    mut label_state: impl FnMut(u32, &str) -> io::Result<crate::scheduled_service::launchd::LabelState>,
) -> io::Result<Value> {
    let directory = expected_directory(home, uuid);
    let _claim = crate::pr_watch_service_install::claim_service_directory(home, &directory, uid)?;
    let record = read_record(home, uuid, uid)?;
    if !record.exists() || (record.unit_present && record.plan_bytes.is_some()) {
        return Err(invalid(
            "GC service record changed while claiming uninstall; run status again",
        ));
    }
    if confirmation.is_none() || confirmation != record.confirmation_digest() {
        return Err(invalid(
            "GC service uninstall requires the exact recorded plan digest (or, with no plan, the unit file's digest)",
        ));
    }
    let state = label_state(uid, &record.label)?;
    if record.dead_reason(state).is_none() {
        return Err(invalid(
            "GC service record is incomplete but launchd did not confirm the label is unloaded; refusing to remove it",
        ));
    }
    let archive = archive_record(&record)?;
    Ok(json!({
        "schema": CONTROL_SCHEMA,
        "action": "uninstall",
        "plan_digest": record.plan_digest,
        "scheduler_state": "not_loaded",
        "installed_files_removed": true,
        "stale_record_archived_to": archive,
        "delivery_proven": false
    }))
}

/// What `service plan` reports about an existing installation record.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn existing_installation_for(plan: &Plan) -> io::Result<Option<Value>> {
    #[cfg(target_os = "macos")]
    let state = crate::scheduled_service::launchd::label_state;
    #[cfg(not(target_os = "macos"))]
    let state = |_: u32, _: &str| {
        Ok::<_, io::Error>(crate::scheduled_service::launchd::LabelState::Unknown)
    };
    existing_installation_with(plan, state)
}

fn existing_installation_with(
    plan: &Plan,
    mut label_state: impl FnMut(u32, &str) -> io::Result<crate::scheduled_service::launchd::LabelState>,
) -> io::Result<Option<Value>> {
    let record = read_record(&plan.home, &plan.repository_uuid, plan.uid)?;
    if !record.exists() {
        return Ok(None);
    }
    let new_bytes = serde_json::to_vec(plan).map_err(io::Error::other)?;
    if record.plan_bytes.as_deref() == Some(new_bytes.as_slice()) {
        return Ok(Some(json!({"same_plan": true})));
    }
    let state = label_state(record.uid, &record.label)?;
    Ok(Some(record.describe(state)))
}

/// What one service refresh did. Written to `last-refresh.json` beside the
/// plan and reported by `update` and `gc service refresh`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RefreshOutcome {
    pub(crate) schema: &'static str,
    pub(crate) repository_uuid: String,
    /// `not_installed`, `not_verifiable`, `already_current`,
    /// `label_state_unknown`, `refresh_failed`, `refresh_failed_restored`,
    /// `refresh_failed_unrestored`, `refreshed_not_loaded`,
    /// `refreshed_and_reloaded`, `refreshed_reload_unconfirmed` or
    /// `refreshed_bootstrap_unconfirmed`.
    pub(crate) outcome: &'static str,
    pub(crate) scheduler_state: &'static str,
    pub(crate) previous_plan_digest: Option<String>,
    pub(crate) previous_runtime_version: Option<String>,
    pub(crate) plan_digest: Option<String>,
    pub(crate) runtime_version: Option<String>,
    pub(crate) archived_to: Option<PathBuf>,
    /// Whether the previous runtime bundle must outlive this update: launchd
    /// may still run it (loaded and not provably reloaded, or unknown).
    pub(crate) keep_previous_runtime: bool,
    /// The version bundle a self-update kept for this service, when it did.
    pub(crate) kept_bundle: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) observed_at_unix_ms: u64,
}

impl RefreshOutcome {
    fn new(uuid: &str) -> Self {
        Self {
            schema: REFRESH_SCHEMA,
            repository_uuid: uuid.to_string(),
            outcome: "refresh_failed",
            scheduler_state: "unknown",
            previous_plan_digest: None,
            previous_runtime_version: None,
            plan_digest: None,
            runtime_version: None,
            archived_to: None,
            // Kept until something proves launchd no longer runs it.
            keep_previous_runtime: true,
            kept_bundle: None,
            error: None,
            observed_at_unix_ms: now_unix_ms().unwrap_or(0),
        }
    }
}

/// Rebind one recorded service to `runtime`, under its installer claim.
///
/// The recorded plan must load and verify (namespace, label, generation,
/// digest, and the unit file's bytes equal to the plan's rendered unit), so
/// nothing a recorded field names is trusted beyond what that check proves.
/// Launchd decides what happens next: unknown changes nothing; not loaded
/// archives the record and publishes the new plan and unit; loaded does the
/// same, then boots the label out and bootstraps the new unit. Never fails:
/// every error lands in the returned outcome.
fn refresh_record_with(
    home: &Path,
    uuid: &str,
    uid: u32,
    runtime: Result<&RuntimePair, &str>,
    mut label_state: impl FnMut(u32, &str) -> io::Result<crate::scheduled_service::launchd::LabelState>,
    mut run: impl FnMut(&[String]) -> io::Result<crate::scheduled_service::launchd::Observation>,
) -> RefreshOutcome {
    let _span = crate::phase_telemetry::span("gc_service_refresh");
    let mut outcome = RefreshOutcome::new(uuid);
    if let Err(error) = refresh_steps(
        home,
        uuid,
        uid,
        runtime,
        &mut label_state,
        &mut run,
        &mut outcome,
    ) {
        outcome.error = Some(bounded_error(&error.to_string()));
    }
    if outcome.outcome != "not_installed" {
        crate::warn_unrecorded(
            "record the GC service refresh",
            write_refresh_receipt(home, uuid, &outcome),
        );
    }
    let counter = match outcome.outcome {
        "refreshed_not_loaded" | "refreshed_and_reloaded" => Some("gc_service_refreshed"),
        "refreshed_reload_unconfirmed" | "refreshed_bootstrap_unconfirmed" => {
            Some("gc_service_refresh_reload_unconfirmed")
        }
        "label_state_unknown" => Some("gc_service_refresh_label_unknown"),
        "refresh_failed"
        | "refresh_failed_restored"
        | "refresh_failed_unrestored"
        | "not_verifiable" => Some("gc_service_refresh_failed"),
        _ => None,
    };
    if let Some(counter) = counter {
        crate::phase_telemetry::add(counter, Duration::ZERO);
    }
    outcome
}

fn refresh_steps(
    home: &Path,
    uuid: &str,
    uid: u32,
    runtime: Result<&RuntimePair, &str>,
    label_state: &mut impl FnMut(u32, &str) -> io::Result<crate::scheduled_service::launchd::LabelState>,
    run: &mut impl FnMut(&[String]) -> io::Result<crate::scheduled_service::launchd::Observation>,
    out: &mut RefreshOutcome,
) -> io::Result<()> {
    use crate::scheduled_service::launchd::LabelState;
    crate::store::repository_identity::validate_uuid_v4(uuid).map_err(io::Error::other)?;
    let directory = expected_directory(home, uuid);
    if !read_record(home, uuid, uid)?.exists() {
        // A refresh that could neither publish nor put the old record back
        // left it in its archive: put it back first, then refresh it.
        let Some(archive) = interrupted_refresh_archive(&directory, uid) else {
            out.outcome = "not_installed";
            out.keep_previous_runtime = false;
            return Ok(());
        };
        let _claim =
            crate::pr_watch_service_install::claim_service_directory(home, &directory, uid)?;
        if read_record(home, uuid, uid)?.exists() {
            return Err(invalid("GC service record changed while claiming refresh"));
        }
        let unit_path = home
            .join(UNIT_DIRECTORY)
            .join(format!("com.aethyme.gc-sweep.{uuid}.plist"));
        restore_record(&archive, &directory, &unit_path, None)?;
    }
    let claim = crate::pr_watch_service_install::claim_service_directory(home, &directory, uid)?;
    out.outcome = "not_verifiable";
    let (plan, bytes) = load(home, uuid)?;
    out.previous_plan_digest = Some(plan.digest.clone());
    out.previous_runtime_version = Some(plan.runtime_version.clone());
    let record = read_record(home, uuid, uid)?;
    if record.plan_bytes.as_deref() != Some(bytes.as_slice()) || !record.unit_present {
        return Err(invalid("GC service record changed while claiming refresh"));
    }
    if let Ok(runtime) = runtime
        && plan.router == runtime.router
        && plan.router_sha256 == runtime.router_sha256
        && plan.engine_sha256 == runtime.engine_sha256
    {
        out.outcome = "already_current";
        out.keep_previous_runtime = false;
        out.scheduler_state = "unchanged";
        out.plan_digest = Some(plan.digest.clone());
        out.runtime_version = Some(plan.runtime_version.clone());
        return Ok(());
    }
    // A probe that could not run proves nothing either way.
    let state = label_state(uid, &plan.label).unwrap_or_else(|error| {
        out.error = Some(bounded_error(&error.to_string()));
        LabelState::Unknown
    });
    out.scheduler_state = label_state_name(state);
    if state == LabelState::Unknown {
        // No proof either way: record the attempt and change nothing.
        out.outcome = "label_state_unknown";
        return Ok(());
    }
    out.outcome = "refresh_failed";
    out.keep_previous_runtime = state == LabelState::Loaded;
    let runtime = runtime.map_err(invalid)?;
    let fresh = Plan::from_runtime(&plan.repository, uuid, &plan.home, uid, runtime)?;
    out.plan_digest = Some(fresh.digest.clone());
    out.runtime_version = Some(fresh.runtime_version.clone());
    let archive = archive_record(&record)?;
    out.archived_to = Some(archive.clone());
    publish_or_restore(&fresh, &record, &archive, &claim, restore_record, out)?;
    if state == LabelState::NotLoaded {
        out.outcome = "refreshed_not_loaded";
        return Ok(());
    }
    reload_refreshed(&fresh, run, out)
}

/// Publish the refreshed plan and unit; when that fails, put the archived
/// record back exactly so a failed refresh never leaves the service with no
/// record at all. If even that fails, `last-refresh.json` says so and names
/// the archive, and `refresh` (which `status` advises) restores it first.
fn publish_or_restore(
    fresh: &Plan,
    record: &InstallationRecord,
    archive: &Path,
    claim: &crate::file_lock::ExclusiveFileLock,
    restore: impl FnOnce(&Path, &Path, &Path, Option<(&[u8], &[u8])>) -> io::Result<()>,
    out: &mut RefreshOutcome,
) -> io::Result<()> {
    let fresh_bytes = serde_json::to_vec(fresh).map_err(io::Error::other)?;
    let Err(publish) = crate::pr_watch_service_install::install_exact_files_claimed(
        &fresh.home,
        &fresh.directory,
        &fresh.unit_path,
        fresh.uid,
        &[("plan.json", fresh_bytes.as_slice())],
        fresh.unit.as_bytes(),
        claim,
    ) else {
        return Ok(());
    };
    match restore(
        archive,
        &record.directory,
        &record.unit_path,
        Some((fresh_bytes.as_slice(), fresh.unit.as_bytes())),
    ) {
        Ok(()) => {
            out.outcome = "refresh_failed_restored";
            out.archived_to = None;
            Err(publish)
        }
        Err(restore) => {
            out.outcome = "refresh_failed_unrestored";
            Err(invalid(format!(
                "publishing the refreshed service failed ({publish}) and restoring the previous record failed ({restore})"
            )))
        }
    }
}

/// Remove `path` only when it holds exactly `bytes` (what a failed publish
/// may have left of the new record).
fn remove_if_exact(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => {
            if fs::read(path)? == bytes {
                fs::remove_file(path)?;
            }
            Ok(())
        }
        Ok(_) => Err(invalid("GC service record holds a non-file path")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn require_absent(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(invalid(
            "GC service restore found a file where the previous record goes",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Move an archived record back: anything exactly matching the failed new
/// record is dropped first, then the plan, then the unit, then the
/// receipts. A crash part-way leaves a plan without its unit, which
/// `broker status` advises about. Nothing that differs is overwritten: the
/// plan and unit refuse a conflict, and a receipt already in the directory
/// (a tick writes `last-error.json` without the installer claim) is newer,
/// so it is kept and the archived copy stays in the archive.
fn restore_record(
    archive: &Path,
    directory: &Path,
    unit_path: &Path,
    fresh: Option<(&[u8], &[u8])>,
) -> io::Result<()> {
    let _span = crate::phase_telemetry::span("gc_service_record_restore");
    if let Some((plan_bytes, unit_bytes)) = fresh {
        remove_if_exact(&directory.join("plan.json"), plan_bytes)?;
        remove_if_exact(unit_path, unit_bytes)?;
    }
    let archived_file = |name: &str| -> io::Result<Option<PathBuf>> {
        let source = archive.join(name);
        match fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.is_file() => Ok(Some(source)),
            Ok(_) => Err(invalid("GC service archive holds a non-file path")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    };
    if let Some(source) = archived_file("plan.json")? {
        let target = directory.join("plan.json");
        require_absent(&target)?;
        fs::rename(&source, &target)?;
        fs::File::open(directory)?.sync_all()?;
    }
    if let Some(source) = archived_file("unit.plist")? {
        require_absent(unit_path)?;
        move_file(&source, unit_path)?;
        if let Some(parent) = unit_path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
    }
    for name in ["control.json", "last-pass.json", "last-error.json"] {
        let Some(source) = archived_file(name)? else {
            continue;
        };
        let target = directory.join(name);
        match fs::symlink_metadata(&target) {
            // Written since the archive: newer than the archived copy.
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::rename(&source, &target)?;
            }
            Err(error) => return Err(error),
        }
    }
    fs::File::open(directory)?.sync_all()?;
    // The emptied archive directory goes; one still holding anything stays.
    if let Err(error) = fs::remove_dir(archive)
        && error.kind() != io::ErrorKind::DirectoryNotEmpty
    {
        crate::warn_unrecorded(
            "remove an emptied GC service record archive",
            Err::<(), _>(error),
        );
    }
    crate::phase_telemetry::add("gc_service_record_restored", Duration::ZERO);
    Ok(())
}

/// The archive an interrupted refresh left the record in, when
/// `last-refresh.json` says so and names a numeric archive of this
/// service's own `archive/` directory (never any other path).
fn interrupted_refresh_archive(directory: &Path, uid: u32) -> Option<PathBuf> {
    let refresh = read_bounded_json(&directory.join("last-refresh.json"), uid)?;
    if refresh.get("outcome").and_then(Value::as_str) != Some("refresh_failed_unrestored") {
        return None;
    }
    let named = PathBuf::from(refresh.get("archived_to")?.as_str()?);
    let stamp = named.file_name()?.to_str()?;
    if stamp.parse::<u64>().is_err() {
        return None;
    }
    let archive = directory.join(RECORD_ARCHIVE_DIRECTORY).join(stamp);
    if named != archive || !fs::symlink_metadata(&archive).is_ok_and(|m| m.is_dir()) {
        return None;
    }
    Some(archive)
}

/// Replace a loaded job with the refreshed unit: bootout, then bootstrap,
/// with a control receipt before and after.
fn reload_refreshed(
    plan: &Plan,
    run: &mut impl FnMut(&[String]) -> io::Result<crate::scheduled_service::launchd::Observation>,
    out: &mut RefreshOutcome,
) -> io::Result<()> {
    use crate::scheduled_service::launchd::Observation;
    let receipt = |scheduler_state: &str, operation_result: &str| -> io::Result<ControlReceipt> {
        Ok(ControlReceipt {
            schema: CONTROL_SCHEMA.to_string(),
            plan_digest: plan.digest.clone(),
            generation: plan.generation.clone(),
            action: "refresh".to_string(),
            scheduler_state: scheduler_state.to_string(),
            operation_result: operation_result.to_string(),
            observed_at_unix_ms: now_unix_ms()?,
        })
    };
    write_control(plan, &receipt("unknown", "attempting")?)?;
    let bootout = run(&[
        "bootout".to_string(),
        format!("gui/{}/{}", plan.uid, plan.label),
    ])?;
    if bootout != Observation::Success {
        // The old job may still be loaded and still names the old runtime.
        write_control(plan, &receipt("unknown", observation_name(&bootout))?)?;
        out.outcome = "refreshed_reload_unconfirmed";
        out.scheduler_state = "unknown";
        return Ok(());
    }
    // The old job is unloaded: nothing runs the previous runtime any more.
    out.keep_previous_runtime = false;
    let bootstrap = run(&[
        "bootstrap".to_string(),
        format!("gui/{}", plan.uid),
        plan.unit_path.to_string_lossy().into_owned(),
    ])?;
    let (scheduler_state, operation_result, outcome) = match bootstrap {
        Observation::Success => ("loaded", "acknowledged", "refreshed_and_reloaded"),
        Observation::Refused => ("unknown", "refused", "refreshed_bootstrap_unconfirmed"),
        Observation::Unknown => ("unknown", "unknown", "refreshed_bootstrap_unconfirmed"),
    };
    write_control(plan, &receipt(scheduler_state, operation_result)?)?;
    out.scheduler_state = scheduler_state;
    out.outcome = outcome;
    Ok(())
}

fn write_bounded_receipt(directory: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(invalid("GC service receipt exceeds its write budget"));
    }
    let target = directory.join(name);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(invalid("GC service receipt path is not a file"));
        }
        _ => {}
    }
    crate::atomic_file::with_synced_temporary(&target, bytes, |temporary| {
        fs::rename(temporary, &target)
    })?;
    fs::File::open(directory)?.sync_all()
}

fn write_refresh_receipt(home: &Path, uuid: &str, outcome: &RefreshOutcome) -> io::Result<()> {
    let directory = expected_directory(home, uuid);
    if !fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.is_dir()) {
        return Ok(());
    }
    let bytes = serde_json::to_vec(outcome).map_err(io::Error::other)?;
    write_bounded_receipt(&directory, "last-refresh.json", &bytes)
}

fn read_bounded_json(path: &Path, uid: u32) -> Option<Value> {
    read_receipt_bytes(path, uid)
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// What a self-update did to the GC services bound to its installation.
#[derive(Debug, Default)]
pub(crate) struct UpdateRefresh {
    /// Version bundles cleanup must keep this time: a service may still run them.
    pub(crate) protected_bundles: Vec<std::ffi::OsString>,
    pub(crate) outcomes: Vec<RefreshOutcome>,
}

/// Refresh every service whose recorded runtime lies in a bundle of
/// `managed_root` other than the one this update or install just selected
/// as `current` (the bundle of `new_router`), or that is already gone.
///
/// A bundle kept as `previous` counts: it still exists, but a managed tick
/// verifies its runtime against the installation's current selection, so a
/// service bound to it refuses every tick until something rebinds it.
/// Called after the selector switch and before cleanup; it never fails the
/// update.
#[cfg(target_os = "macos")]
pub(crate) fn refresh_for_update(managed_root: &Path, new_router: &Path) -> UpdateRefresh {
    let _span = crate::phase_telemetry::span("gc_service_refresh_for_update");
    let Some(active) = new_router.parent().and_then(Path::file_name) else {
        return UpdateRefresh::default();
    };
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return UpdateRefresh::default();
    };
    let Ok(home) = absolute_directory(&home) else {
        return UpdateRefresh::default();
    };
    // SAFETY: geteuid has no preconditions and reads this process's effective user ID.
    let uid = unsafe { libc::geteuid() };
    if uid == 0 {
        return UpdateRefresh::default();
    }
    refresh_for_update_with(
        &home,
        uid,
        managed_root,
        active,
        || runtime_pair(new_router),
        crate::scheduled_service::launchd::label_state,
        launchctl,
    )
}

/// The whole walk inside one self-update gets this long; services not
/// reached in time keep their bundle and are picked up later.
const UPDATE_REFRESH_BUDGET: Duration = Duration::from_secs(30);

fn refresh_for_update_with(
    home: &Path,
    uid: u32,
    managed_root: &Path,
    active: &std::ffi::OsStr,
    runtime: impl FnOnce() -> io::Result<RuntimePair>,
    label_state: impl FnMut(u32, &str) -> io::Result<crate::scheduled_service::launchd::LabelState>,
    run: impl FnMut(&[String]) -> io::Result<crate::scheduled_service::launchd::Observation>,
) -> UpdateRefresh {
    refresh_for_update_within(
        home,
        uid,
        managed_root,
        active,
        std::time::Instant::now() + UPDATE_REFRESH_BUDGET,
        runtime,
        label_state,
        run,
    )
}

#[allow(clippy::too_many_arguments)]
fn refresh_for_update_within(
    home: &Path,
    uid: u32,
    managed_root: &Path,
    active: &std::ffi::OsStr,
    deadline: std::time::Instant,
    runtime: impl FnOnce() -> io::Result<RuntimePair>,
    mut label_state: impl FnMut(u32, &str) -> io::Result<crate::scheduled_service::launchd::LabelState>,
    mut run: impl FnMut(&[String]) -> io::Result<crate::scheduled_service::launchd::Observation>,
) -> UpdateRefresh {
    let mut report = UpdateRefresh::default();
    let Ok(entries) = fs::read_dir(home.join(SERVICE_DIRECTORY)) else {
        return report;
    };
    let mut uuids: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| crate::store::repository_identity::validate_uuid_v4(name).is_ok())
        .collect();
    uuids.sort();
    let versions = managed_root.join("versions");
    let mut runtime = Some(runtime);
    let mut resolved: Option<Result<RuntimePair, String>> = None;
    for uuid in uuids {
        let Ok(record) = read_record(home, &uuid, uid) else {
            continue;
        };
        // The recorded router only selects candidates and names the bundle
        // to keep; the refresh itself acts on the verified plan.
        let Some(router) = record.router.as_deref() else {
            continue;
        };
        let Some(version_directory) = router.parent() else {
            continue;
        };
        if version_directory.parent() != Some(versions.as_path()) {
            continue;
        }
        let Some(bundle) = version_directory.file_name() else {
            continue;
        };
        if !needs_update_refresh(bundle, active, router.exists()) {
            continue;
        }
        if std::time::Instant::now() >= deadline {
            // Out of budget: launchd was not asked, so this is unknown. The
            // bundle stays; the next update or `gc service status` retries.
            let mut outcome = RefreshOutcome::new(&uuid);
            outcome.outcome = "label_state_unknown";
            outcome.error = Some("the update's GC service refresh budget was spent".into());
            outcome.kept_bundle = bundle.to_str().map(str::to_string);
            crate::warn_unrecorded(
                "record the GC service refresh",
                write_refresh_receipt(home, &uuid, &outcome),
            );
            crate::phase_telemetry::add("gc_service_refresh_budget_spent", Duration::ZERO);
            report.protected_bundles.push(bundle.to_os_string());
            report.outcomes.push(outcome);
            continue;
        }
        let pair = resolved.get_or_insert_with(|| match runtime.take() {
            Some(resolve) => resolve().map_err(|error| error.to_string()),
            None => Err("the new runtime pair was not resolved".to_string()),
        });
        let outcome = refresh_record_with(
            home,
            &uuid,
            uid,
            pair.as_ref().map_err(String::as_str),
            &mut label_state,
            &mut run,
        );
        let mut outcome = outcome;
        if outcome.keep_previous_runtime {
            outcome.kept_bundle = bundle.to_str().map(str::to_string);
            report.protected_bundles.push(bundle.to_os_string());
        }
        report.outcomes.push(outcome);
    }
    report
}

/// `gc service refresh`: rebind this repository's service to the running
/// installer-managed runtime.
#[cfg(target_os = "macos")]
pub(crate) fn refresh_current(home: &Path, uuid: &str) -> RefreshOutcome {
    // SAFETY: geteuid has no preconditions and reads this process's effective user ID.
    let uid = unsafe { libc::geteuid() };
    let runtime = std::env::current_exe()
        .and_then(|current| runtime_pair(&current))
        .map_err(|error| error.to_string());
    refresh_record_with(
        home,
        uuid,
        uid,
        runtime.as_ref().map_err(String::as_str),
        crate::scheduled_service::launchd::label_state,
        launchctl,
    )
}

/// Whether an update that just selected bundle `active` must rebind a
/// service whose recorded router lies in `bundle`: every other bundle,
/// including the one kept as `previous`, and any bundle whose router is gone.
fn needs_update_refresh(
    bundle: &std::ffi::OsStr,
    active: &std::ffi::OsStr,
    router_present: bool,
) -> bool {
    bundle != active || !router_present
}

/// How a recorded service's runtime relates to its installation's current
/// selection. A managed tick only runs the pair `current` selects (its
/// preflight verifies the recorded router as the installer receipt's
/// router), so a present runtime that is no longer current refuses every
/// tick just as a removed one does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeBinding {
    /// The recorded router is the one `current` selects.
    Current,
    /// The recorded router exists but `current` selects another one.
    Stale,
    /// The recorded router is gone.
    Gone,
    /// The installation's `current` selector cannot be read: proves nothing.
    Unknown,
}

impl RuntimeBinding {
    fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Stale => "stale",
            Self::Gone => "gone",
            Self::Unknown => "unknown",
        }
    }

    /// Whether a refresh to the current runtime is what this binding needs.
    fn needs_refresh(self) -> bool {
        matches!(self, Self::Stale | Self::Gone)
    }
}

/// The router the installation rooted at `managed_root` currently selects,
/// resolved through its `current` selector. A few `lstat`/`readlink` calls:
/// no process, no launchd, no hashing.
fn current_selection(managed_root: &Path) -> Option<PathBuf> {
    fs::canonicalize(managed_root.join("current").join("aethyme")).ok()
}

/// Classify `router` against the selection of `managed_root`.
fn runtime_binding(router: &Path, managed_root: &Path) -> RuntimeBinding {
    if !router.exists() {
        return RuntimeBinding::Gone;
    }
    let Ok(router) = fs::canonicalize(router) else {
        return RuntimeBinding::Unknown;
    };
    match current_selection(managed_root) {
        Some(current) if current == router => RuntimeBinding::Current,
        Some(_) => RuntimeBinding::Stale,
        None => RuntimeBinding::Unknown,
    }
}

/// Whether `running` is the router `managed_root` currently selects: the
/// only runtime a foreground command may rebind a service to by itself.
fn running_is_current_selection(running: &Path, managed_root: &Path) -> bool {
    match (fs::canonicalize(running), current_selection(managed_root)) {
        (Ok(running), Some(current)) => running == current,
        _ => false,
    }
}

/// Whether the service's ticks are refusing: its last recorded tick error
/// is newer than its last finished pass (or there is no pass at all).
fn tick_refusing(
    last_error_at_unix_ms: Option<u64>,
    last_pass_finished_unix_ms: Option<u64>,
) -> bool {
    match (last_error_at_unix_ms, last_pass_finished_unix_ms) {
        (Some(error), Some(pass)) => error > pass,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// When the service last recorded a tick error and finished a pass, read
/// from its receipts without requiring the plan to verify.
fn tick_timestamps(directory: &Path, uid: u32) -> (Option<u64>, Option<u64>) {
    let field = |name: &str, key: &str| {
        read_bounded_json(&directory.join(name), uid)
            .and_then(|value| value.get(key).and_then(Value::as_u64))
    };
    (
        field("last-error.json", "observed_at_unix_ms"),
        field("last-pass.json", "finished_at_unix_ms"),
    )
}

/// Whether a foreground command may refresh this repository's service by
/// itself: the recorded runtime is gone or no longer the installation's
/// current selection, the command runs from exactly that current selection
/// (the runtime the refresh binds), the uuid read from the repository's
/// marker is the service's, the recorded plan verifies (its label is
/// exactly `com.aethyme.gc-sweep.<uuid>` and the unit file's bytes equal
/// its rendered unit), and it names this repository.
fn automatic_refresh_proof(
    home: &Path,
    uuid: &str,
    marker_uuid: &str,
    repository: &Path,
    running: &Path,
) -> Result<(), &'static str> {
    if uuid != marker_uuid {
        return Err("service_uuid_differs_from_repository_marker");
    }
    let (plan, _) = load(home, uuid).map_err(|_| "recorded_plan_or_unit_not_verified")?;
    if plan.label != format!("com.aethyme.gc-sweep.{uuid}") {
        return Err("label_differs");
    }
    if plan.repository != repository {
        return Err("plan_names_another_repository");
    }
    match runtime_binding(&plan.router, &plan.managed_root) {
        RuntimeBinding::Stale | RuntimeBinding::Gone => {}
        RuntimeBinding::Current => return Err("recorded_runtime_current"),
        RuntimeBinding::Unknown => return Err("installation_selection_unreadable"),
    }
    if !running_is_current_selection(running, &plan.managed_root) {
        return Err("running_runtime_is_not_the_current_selection");
    }
    Ok(())
}

/// What `broker status` did about a stale or gone service binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScheduledRefresh {
    /// A detached `gc service status` was started to refresh the service.
    Started,
    /// One was started (or a refresh was attempted) within the last
    /// interval: nothing is started again until it passes.
    RecentlyAttempted,
}

impl ScheduledRefresh {
    /// The evidence line `broker status` adds to the service's advice.
    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::Started => {
                "a background `aethyme broker gc service status --json` was started to rebind the service to the current runtime"
            }
            Self::RecentlyAttempted => {
                "a refresh was started or attempted within the last 15 minutes; the next one waits for that interval"
            }
        }
    }
}

/// The receipt `broker status` writes before it starts a background refresh,
/// so at most one is started per service interval.
const SCHEDULED_REFRESH_RECEIPT: &str = "refresh-scheduled.json";
const SCHEDULED_REFRESH_SCHEMA: &str = "aethyme.gc-sweep-service-refresh-scheduled/v1";

/// `broker status` noticing a stale binding: when this repository's
/// recorded service names a runtime that is gone or no longer current, and
/// the command runs from the installation's current selection, write a
/// bounded receipt and `start` a background refresh, at most once per
/// [`SERVICE_INTERVAL_SECONDS`] (counting refreshes attempted by any path).
/// Files and selector links only; launchd is asked by the refresh itself,
/// which repeats every proof under the service's installer claim. `None`
/// when the binding needs nothing or nothing could be proven.
fn schedule_refresh_with(
    home: &Path,
    uuid: &str,
    uid: u32,
    running: &Path,
    now_unix_ms: u64,
    start: impl FnOnce() -> io::Result<()>,
) -> Option<ScheduledRefresh> {
    let _span = crate::phase_telemetry::span("gc_service_refresh_schedule");
    let record = read_record(home, uuid, uid).ok()?;
    if record.plan_bytes.is_none() || !record.unit_present {
        return None;
    }
    let router = record.router.as_deref()?;
    let managed_root = record.managed_root.as_deref()?;
    if !runtime_binding(router, managed_root).needs_refresh()
        || !running_is_current_selection(running, managed_root)
    {
        return None;
    }
    let attempted = |name: &str| {
        read_bounded_json(&record.directory.join(name), uid)
            .and_then(|value| value.get("observed_at_unix_ms").and_then(Value::as_u64))
    };
    let last = attempted(SCHEDULED_REFRESH_RECEIPT).max(attempted("last-refresh.json"));
    if last.is_some_and(|at| now_unix_ms.saturating_sub(at) < SERVICE_INTERVAL_SECONDS * 1000) {
        return Some(ScheduledRefresh::RecentlyAttempted);
    }
    // The receipt goes first: without it nothing bounds the next start.
    let receipt = json!({
        "schema": SCHEDULED_REFRESH_SCHEMA,
        "repository_uuid": uuid,
        "observed_at_unix_ms": now_unix_ms,
    });
    let bytes = serde_json::to_vec(&receipt).ok()?;
    write_bounded_receipt(&record.directory, SCHEDULED_REFRESH_RECEIPT, &bytes).ok()?;
    match start() {
        Ok(()) => {
            crate::phase_telemetry::add("gc_service_refresh_scheduled", Duration::ZERO);
            Some(ScheduledRefresh::Started)
        }
        Err(error) => {
            crate::warn_unrecorded(
                "start the background GC service refresh",
                Err::<(), _>(error),
            );
            None
        }
    }
}

/// `broker status` on macOS: see [`schedule_refresh_with`]. The background
/// refresh is `<running> broker gc service status --json` in `repository`,
/// detached from the caller's process group with no stdio, whose automatic
/// refresh repeats every proof before it touches launchd.
#[cfg(target_os = "macos")]
pub(crate) fn schedule_refresh_if_stale(
    home: &Path,
    uuid: &str,
    uid: u32,
    repository: &Path,
) -> Option<ScheduledRefresh> {
    use std::os::unix::process::CommandExt as _;
    let running = std::env::current_exe().ok()?;
    let now = now_unix_ms().ok()?;
    schedule_refresh_with(home, uuid, uid, &running, now, || {
        let mut command = std::process::Command::new(&running);
        command
            .args(["broker", "gc", "service", "status", "--json"])
            .current_dir(repository)
            .env_remove(SERVICE_ENV_UUID)
            .env_remove(SERVICE_ENV_GENERATION)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        let mut child = command.spawn()?;
        // Reap it whenever it ends; a short-lived caller simply exits first.
        std::thread::spawn(move || {
            crate::warn_unrecorded("reap the background GC service refresh", child.wait());
        });
        Ok(())
    })
}

/// A `broker status` advice row about this repository's scheduled GC
/// service: each is a way for the service to be silently gone.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct RepairAdvice {
    pub(crate) id: &'static str,
    pub(crate) reason: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) evidence: Vec<String>,
    pub(crate) commands: Vec<String>,
}

/// Whether the last control receipt meant the job to be loaded and nothing
/// confirmed it: an activation or refresh refused, unknown or interrupted.
fn load_unconfirmed(control: &ControlReceipt) -> bool {
    matches!(control.action.as_str(), "activate" | "refresh")
        && !matches!(
            control.operation_result.as_str(),
            "acknowledged" | "observed_loaded"
        )
}

/// What `broker status` should say about this repository's service, from
/// files only (no launchctl): the recorded runtime is gone; a refresh left
/// the record in its archive; or a load was meant and never confirmed.
pub(crate) fn repair_advice(home: &Path, uuid: &str, uid: u32) -> Option<RepairAdvice> {
    let record = read_record(home, uuid, uid).ok()?;
    if !record.exists() {
        let archive = interrupted_refresh_archive(&record.directory, uid)?;
        return Some(RepairAdvice {
            id: "storage.gc-service-record-interrupted",
            reason: "a refresh archived the scheduled GC service's record, could not publish the new one and could not put the old one back; launchd may still hold the job, but no record remains",
            summary: "this repository's scheduled GC sweep service record was left in its archive by a failed refresh",
            evidence: vec![format!("archived record: {}", archive.display())],
            commands: vec![REFRESH_COMMAND.into()],
        });
    }
    if record.plan_bytes.is_some() && record.runtime_present() == Some(false) {
        let mut evidence = vec![format!(
            "the recorded runtime {} is no longer installed; launchd cannot start the job",
            record.runtime_version.as_deref().unwrap_or("unknown")
        )];
        if !record.unit_present {
            evidence.push("the unit file is missing too".into());
        }
        if let Some(refresh) = read_bounded_json(&record.directory.join("last-refresh.json"), uid)
            && let Some(outcome) = refresh.get("outcome").and_then(Value::as_str)
        {
            evidence.push(format!("last refresh attempt: {outcome}"));
        }
        return Some(RepairAdvice {
            id: "storage.gc-service-runtime-missing",
            reason: "the scheduled GC sweep service is pinned to one exact runtime, and self-update removed it; launchd cannot start the job, so scheduled sweeps have stopped",
            summary: "this repository's scheduled GC sweep service names a runtime that is no longer installed",
            evidence,
            commands: vec![
                REFRESH_COMMAND.into(),
                "aethyme broker gc service status --json".into(),
            ],
        });
    }
    if record.plan_bytes.is_some() && !record.unit_present {
        return Some(RepairAdvice {
            id: "storage.gc-service-unit-missing",
            reason: "the scheduled GC sweep service has its plan but no unit file, so launchd has nothing to load after a logout or restart; install replaces such a record once launchd confirms the label is not loaded",
            summary: "this repository's scheduled GC sweep service has a plan but no unit file",
            evidence: vec![format!(
                "plan {} for runtime {} is recorded; the unit file is missing",
                record.plan_digest.as_deref().unwrap_or("unreadable"),
                record.runtime_version.as_deref().unwrap_or("unknown")
            )],
            commands: vec![
                "aethyme broker gc service status --json".into(),
                "aethyme broker gc service plan".into(),
            ],
        });
    }
    if let Some(advice) = refusing_or_stale_advice(&record, uid) {
        return Some(advice);
    }
    let (plan, _) = load(home, uuid).ok()?;
    let control = read_control(&plan).ok().flatten()?;
    if !load_unconfirmed(&control) {
        return None;
    }
    Some(RepairAdvice {
        id: "storage.gc-service-load-unconfirmed",
        reason: "the scheduled GC sweep service was meant to be loaded, but launchd never confirmed it (a refused or unknown bootstrap); if it is not loaded, scheduled sweeps have stopped",
        summary: "this repository's scheduled GC sweep service was meant to be loaded and launchd did not confirm it",
        evidence: vec![format!(
            "last control: {} {} (scheduler {})",
            control.action, control.operation_result, control.scheduler_state
        )],
        commands: vec![
            "aethyme broker gc service status --json".into(),
            format!(
                "aethyme broker gc service activate --confirm {}",
                plan.digest
            ),
        ],
    })
}

/// The advice for a recorded service whose runtime is present but no longer
/// the installation's current selection, or whose ticks refuse (the last
/// tick error is newer than the last finished pass). Files and selector
/// links only.
fn refusing_or_stale_advice(record: &InstallationRecord, uid: u32) -> Option<RepairAdvice> {
    record.plan_bytes.as_ref()?;
    let binding = match (record.router.as_deref(), record.managed_root.as_deref()) {
        (Some(router), Some(root)) => runtime_binding(router, root),
        _ => RuntimeBinding::Unknown,
    };
    let (last_error_at, last_pass_at) = tick_timestamps(&record.directory, uid);
    let refusing = tick_refusing(last_error_at, last_pass_at);
    if binding != RuntimeBinding::Stale && !refusing {
        return None;
    }
    let mut evidence = Vec::new();
    if binding == RuntimeBinding::Stale {
        evidence.push(format!(
            "the recorded runtime {} is installed but no longer the installation's current one; managed ticks refuse it",
            record.runtime_version.as_deref().unwrap_or("unknown")
        ));
    }
    if refusing
        && let Some(error) = read_bounded_json(&record.directory.join("last-error.json"), uid)
    {
        evidence.push(format!(
            "last tick refused at (ms since epoch) {}: {}",
            last_error_at.unwrap_or(0),
            error
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ));
        evidence.push(match last_pass_at {
            Some(at) => format!("last finished pass at (ms since epoch): {at}"),
            None => "no finished pass is recorded for this plan".into(),
        });
    }
    if let Some(refresh) = read_bounded_json(&record.directory.join("last-refresh.json"), uid)
        && let Some(outcome) = refresh.get("outcome").and_then(Value::as_str)
    {
        evidence.push(format!("last refresh attempt: {outcome}"));
    }
    Some(if binding == RuntimeBinding::Stale {
        RepairAdvice {
            id: "storage.gc-service-runtime-stale",
            reason: "the scheduled GC sweep service is pinned to one exact runtime, and an update selected another one; each tick verifies its runtime as the installation's current one and refuses, so scheduled sweeps have stopped",
            summary: "this repository's scheduled GC sweep service is bound to a runtime that is no longer the current one",
            evidence,
            commands: vec![
                REFRESH_COMMAND.into(),
                "aethyme broker gc service status --json".into(),
            ],
        }
    } else {
        RepairAdvice {
            id: "storage.gc-service-ticks-refusing",
            reason: "the scheduled GC sweep service's last tick refused before sweeping and no pass has finished since, so scheduled sweeps have stopped",
            summary: "this repository's scheduled GC sweep service refuses its ticks",
            evidence,
            commands: vec![
                REFRESH_COMMAND.into(),
                "aethyme broker gc service status --json".into(),
            ],
        }
    })
}

/// What `broker status` should say when the service runs but its passes
/// have reclaimed nothing for [`UNRECLAIMED_PASSES_ADVICE_THRESHOLD`] passes
/// while the last one saw reclaimable work: the cause it recorded and the
/// command that acts on it. Files only (no launchctl).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn reclaim_advice(home: &Path, uuid: &str) -> Option<RepairAdvice> {
    let (plan, _) = load(home, uuid).ok()?;
    let receipt = read_last_pass(&plan).ok().flatten()?;
    reclaim_advice_for(&receipt)
}

fn reclaim_advice_for(receipt: &LastPassReceipt) -> Option<RepairAdvice> {
    if receipt.passes_without_reclaim < UNRECLAIMED_PASSES_ADVICE_THRESHOLD {
        return None;
    }
    let cause = receipt.reclaimed_nothing_because.as_deref()?;
    // Only causes a command can act on: a lane that yielded to the detached
    // worker or was disabled on purpose, and trash rows the proofs keep, are
    // explained in the receipt and need nobody.
    let (reason, command): (&'static str, &str) = match cause {
        "trash_not_draining" => (
            "confirmed checkout trash entries are waiting and the backlog has not shrunk between passes: the paced host deleter is not getting to them",
            "aethyme broker gc trash drain --json",
        ),
        "artifact_lane_budget_spent" => (
            "the artifact cache lane keeps stopping at its budget with work left and reclaims nothing",
            "aethyme broker gc sweep --json",
        ),
        _ => return None,
    };
    let mut evidence = vec![format!(
        "{} consecutive passes reclaimed nothing; recorded cause: {cause}",
        receipt.passes_without_reclaim
    )];
    if let Some(since) = receipt.unreclaimed_since_unix_ms {
        evidence.push(format!(
            "first empty pass started at (ms since epoch): {since}"
        ));
    }
    if let Some(trash) = receipt.trash {
        evidence.push(format!(
            "checkout trash: {} entries recorded, {} deletable, {} unconfirmed ({} stale); held back: {} identity changed, {} failing, {} in use",
            trash.recorded,
            trash.deletable,
            trash.pending,
            trash.stale_pending,
            trash.identity_changed,
            trash.failing,
            trash.in_use
        ));
    }
    if let Some(lane) = &receipt.artifact_lane {
        evidence.push(format!("artifact cache lane: {lane}"));
    }
    Some(RepairAdvice {
        id: "storage.gc-service-reclaiming-nothing",
        reason,
        summary: "this repository's scheduled GC sweep has reclaimed nothing for at least two hours while reclaimable storage remains",
        evidence,
        commands: vec![
            command.to_string(),
            "aethyme broker gc service status --json".into(),
        ],
    })
}

/// `gc service status` retries the bootstrap of a service whose last
/// activation or refresh was never confirmed, under the same proofs as the
/// automatic refresh (marker uuid, verified plan and unit, this repository,
/// runtime present) and only when launchd says the label is not loaded.
fn retry_unconfirmed_load_with(
    home: &Path,
    uuid: &str,
    marker_uuid: &str,
    repository: &Path,
    mut label_state: impl FnMut(u32, &str) -> io::Result<crate::scheduled_service::launchd::LabelState>,
    mut run: impl FnMut(&[String]) -> io::Result<crate::scheduled_service::launchd::Observation>,
) -> io::Result<Option<Value>> {
    use crate::scheduled_service::launchd::{LabelState, Observation};
    if uuid != marker_uuid {
        return Ok(None);
    }
    let Ok((plan, bytes)) = load(home, uuid) else {
        return Ok(None);
    };
    if plan.repository != repository || !plan.router.exists() {
        return Ok(None);
    }
    if !read_control(&plan)
        .ok()
        .flatten()
        .is_some_and(|control| load_unconfirmed(&control))
    {
        return Ok(None);
    }
    let _span = crate::phase_telemetry::span("gc_service_load_retry");
    let _claim = crate::pr_watch_service_install::claim_installer(&plan.directory, plan.uid)?;
    if load(home, uuid)?.1 != bytes {
        return Err(invalid(
            "GC service installation changed while claiming a retry",
        ));
    }
    let state = label_state(plan.uid, &plan.label).unwrap_or(LabelState::Unknown);
    let receipt = |scheduler_state: &str, operation_result: &str| -> io::Result<ControlReceipt> {
        Ok(ControlReceipt {
            schema: CONTROL_SCHEMA.to_string(),
            plan_digest: plan.digest.clone(),
            generation: plan.generation.clone(),
            action: "activate".to_string(),
            scheduler_state: scheduler_state.to_string(),
            operation_result: operation_result.to_string(),
            observed_at_unix_ms: now_unix_ms()?,
        })
    };
    match state {
        LabelState::Unknown => {
            return Ok(Some(
                json!({"retried": false, "scheduler_state": "unknown"}),
            ));
        }
        LabelState::Loaded => {
            // Loaded after all: record the observation so the advice clears.
            write_control(&plan, &receipt("loaded", "observed_loaded")?)?;
            return Ok(Some(json!({"retried": false, "scheduler_state": "loaded"})));
        }
        LabelState::NotLoaded => {}
    }
    write_control(&plan, &receipt("unknown", "attempting")?)?;
    let observation = run(&[
        "bootstrap".to_string(),
        format!("gui/{}", plan.uid),
        plan.unit_path.to_string_lossy().into_owned(),
    ])?;
    let (scheduler_state, operation_result) = match observation {
        Observation::Success => ("loaded", "acknowledged"),
        Observation::Refused => ("unknown", "refused"),
        Observation::Unknown => ("unknown", "unknown"),
    };
    write_control(&plan, &receipt(scheduler_state, operation_result)?)?;
    crate::phase_telemetry::add("gc_service_load_retried", Duration::ZERO);
    Ok(Some(json!({
        "retried": true,
        "scheduler_state": scheduler_state,
        "operation_result": operation_result
    })))
}

/// Record why a managed tick refused to run, beside `last-pass.json`, so a
/// failing service is not silent (its output goes to /dev/null).
#[cfg(target_os = "macos")]
pub(crate) fn record_tick_failure(error: &str) {
    let Ok(uuid) = std::env::var(SERVICE_ENV_UUID) else {
        return;
    };
    let generation = std::env::var(SERVICE_ENV_GENERATION).ok();
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    // SAFETY: geteuid has no preconditions and reads this process's effective user ID.
    let uid = unsafe { libc::geteuid() };
    crate::warn_unrecorded(
        "record the GC service tick failure",
        write_tick_failure(&home, &uuid, uid, generation.as_deref(), error),
    );
}

fn write_tick_failure(
    home: &Path,
    uuid: &str,
    uid: u32,
    generation: Option<&str>,
    error: &str,
) -> io::Result<()> {
    crate::store::repository_identity::validate_uuid_v4(uuid).map_err(io::Error::other)?;
    let home = absolute_directory(home)?;
    let directory = expected_directory(&home, uuid);
    crate::pr_watch_service_install::lifecycle::check_directories(&home, &directory, uid, true)?;
    let receipt = json!({
        "schema": ERROR_SCHEMA,
        "repository_uuid": uuid,
        "generation": generation.filter(|value| valid_hex_digest(value)),
        "observed_at_unix_ms": now_unix_ms()?,
        "error": bounded_error(error),
    });
    let bytes = serde_json::to_vec(&receipt).map_err(io::Error::other)?;
    write_bounded_receipt(&directory, "last-error.json", &bytes)?;
    crate::phase_telemetry::add("gc_service_tick_refused", Duration::ZERO);
    Ok(())
}

#[cfg(target_os = "macos")]
pub(crate) fn control_home() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| invalid("HOME is required for GC service control"))?;
    absolute_directory(&home)
}

#[cfg(target_os = "macos")]
pub(crate) fn uuid_for_current_repository() -> io::Result<String> {
    current_repository().map(|(_, uuid)| uuid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduled_service::launchd::Observation;
    use std::os::unix::fs::PermissionsExt;

    fn fixture() -> (tempfile::TempDir, Plan) {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("repo");
        let version_directory = temp
            .path()
            .join("install/.aethyme-managed/versions/v0.8.50-test");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&version_directory).unwrap();
        let router = version_directory.join("aethyme");
        let engine = version_directory.join("aethyme-engine-cli");
        fs::write(&router, b"router fixture").unwrap();
        fs::write(&engine, b"engine fixture").unwrap();
        fs::set_permissions(&router, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&engine, fs::Permissions::from_mode(0o700)).unwrap();
        let managed_root = temp.path().join("install/.aethyme-managed");
        let runtime = RuntimePair {
            managed_root,
            version_directory,
            runtime_version: "0.8.50".into(),
            runtime_build_identity: "0.8.50 (build_revision=fixture)".into(),
            router: router.clone(),
            engine: engine.clone(),
            router_sha256: binary_digest(&router).unwrap(),
            engine_sha256: binary_digest(&engine).unwrap(),
        };
        let plan = Plan::from_runtime(
            &repo,
            "123e4567-e89b-42d3-a456-426614174000",
            &home,
            // SAFETY: geteuid has no preconditions and reads this process's effective user ID.
            unsafe { libc::geteuid() },
            &runtime,
        )
        .unwrap();
        (temp, plan)
    }

    fn install_fixture(plan: &Plan) {
        install_exact_plan_files(plan).unwrap();
    }

    #[test]
    fn plan_digest_binds_both_binaries_and_fixed_public_tick() {
        let (_temp, plan) = fixture();
        assert_eq!(plan.interval_seconds, 900);
        assert!(plan.unit.contains("<string>broker</string><string>gc</string><string>sweep</string><string>--json</string>"));
        assert!(plan.unit.contains("<key>RunAtLoad</key><false/>"));
        assert!(
            plan.unit
                .contains("<key>AETHYME_GC_SWEEP_SERVICE_GENERATION</key>")
        );
        assert_eq!(plan.router_sha256.len(), 64);
        assert_eq!(plan.engine_sha256.len(), 64);
        let encoded = serde_json::to_vec(&plan).unwrap();
        validate_plan(&plan, &encoded, &plan.home, &plan.repository_uuid, plan.uid).unwrap();
        let mut changed = plan.clone();
        let replacement = if changed.engine_sha256.starts_with('0') {
            "1"
        } else {
            "0"
        };
        changed.engine_sha256.replace_range(0..1, replacement);
        assert!(
            validate_plan(
                &changed,
                &serde_json::to_vec(&changed).unwrap(),
                &plan.home,
                &plan.repository_uuid,
                plan.uid
            )
            .is_err()
        );
    }

    #[test]
    fn service_install_is_inert_and_uninstall_requires_acknowledged_deactivation() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let mut calls = 0;
        assert!(uninstall_loaded(&plan, &serde_json::to_vec(&plan).unwrap()).is_err());
        let result = control_with(
            &plan.home,
            &plan.repository_uuid,
            "deactivate",
            Some(&plan.digest),
            |args| {
                calls += 1;
                assert_eq!(args[0], "bootout");
                Ok(Observation::Success)
            },
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(result["scheduler_state"], "unloaded");
        let result = uninstall_loaded(&plan, &serde_json::to_vec(&plan).unwrap()).unwrap();
        assert_eq!(result["installed_files_removed"], true);
        assert!(!plan.unit_path.exists());
        assert!(!plan.directory.join("plan.json").exists());
    }

    #[test]
    fn unsafe_receipts_and_unacknowledged_deactivation_are_not_absence() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        assert!(read_control(&plan).unwrap().is_none());
        fs::create_dir(plan.directory.join("control.json")).unwrap();
        assert!(read_control(&plan).is_err());
        fs::remove_dir(plan.directory.join("control.json")).unwrap();
        let result = control_with(
            &plan.home,
            &plan.repository_uuid,
            "deactivate",
            Some(&plan.digest),
            |_| Ok(Observation::Unknown),
        )
        .unwrap();
        assert_eq!(result["scheduler_state"], "unknown");
        assert!(uninstall_loaded(&plan, &serde_json::to_vec(&plan).unwrap()).is_err());
        assert!(plan.unit_path.exists());
        assert!(install(&plan, "different-digest").is_err());
    }

    #[test]
    fn tick_receipts_are_bounded_and_do_not_claim_backlog_delivery() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        write_last_pass(&plan, 1, None, Some(&"x".repeat(32_768))).unwrap();
        assert!(fs::metadata(&plan.receipt_path).unwrap().len() <= MAX_RECEIPT_BYTES as u64);
        let receipt = read_last_pass(&plan).unwrap().unwrap();
        assert_eq!(receipt.outcome, "error");
        assert_eq!(receipt.error.unwrap().len(), 256);
        assert!(receipt.directories_reclaimed.is_none());
        let report = crate::ArtifactSweepReport {
            at_ms: 1,
            scope: "inline".into(),
            directories_reclaimed: 0,
            entries_reclaimed: 0,
            observed_logical_file_bytes: 0,
            preparation_cache_bytes_reclaimed: 0,
            bytes_accounting_complete: true,
            blockers: crate::ArtifactSweepBlockerSummary::default(),
            headroom_threshold_bytes: crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES,
            complete: false,
            budget_ms: 250,
            disk_pressured: false,
            legacy_unscoped_state: crate::ArtifactSweepLegacyUnscopedState::UnknownPending,
            artifact_lane: crate::ArtifactSweepLane::Ran,
            checkout_trash: crate::CheckoutTrashBacklog::default(),
            auto_cleanup: None,
        };
        write_last_pass(&plan, 2, Some(&report), None).unwrap();
        let receipt = read_last_pass(&plan).unwrap().unwrap();
        assert_eq!(
            receipt.legacy_unscoped_state.as_deref(),
            Some("unknown_pending")
        );
        assert_eq!(receipt.outcome, "sweep_paused");
    }

    fn empty_pass(
        lane: crate::ArtifactSweepLane,
        trash: crate::CheckoutTrashBacklog,
    ) -> crate::ArtifactSweepReport {
        crate::ArtifactSweepReport {
            at_ms: 1,
            scope: "drain".into(),
            directories_reclaimed: 0,
            entries_reclaimed: 0,
            observed_logical_file_bytes: 0,
            preparation_cache_bytes_reclaimed: 0,
            bytes_accounting_complete: true,
            blockers: crate::ArtifactSweepBlockerSummary::default(),
            headroom_threshold_bytes: crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES,
            complete: false,
            budget_ms: 5000,
            disk_pressured: false,
            legacy_unscoped_state: crate::ArtifactSweepLegacyUnscopedState::Clear,
            artifact_lane: lane,
            checkout_trash: trash,
            auto_cleanup: None,
        }
    }

    /// Entries that leave the deletable backlog by changing identity,
    /// reaching the failure cap or being in use are held back, never counted
    /// as drained: a backlog that only lost such entries is not draining,
    /// even when its oldest entry moved forward.
    #[test]
    fn entries_held_back_are_not_counted_as_drained() {
        let previous = trash(20, Some(5), 0);
        let held = crate::CheckoutTrashBacklog {
            identity_changed: 3,
            identity_changed_device: 3,
            failing: 2,
            in_use: 1,
            ..trash(14, Some(9), 0)
        };
        let drain = trash_drain_since(Some(&previous), &held).unwrap();
        assert_eq!(drain.entries, 0, "{drain:?}");
        assert_eq!(drain.held_back, 6, "{drain:?}");
        assert!(!drain.draining, "{drain:?}");

        let deleted_too = crate::CheckoutTrashBacklog {
            deletable: 10,
            ..held
        };
        let drain = trash_drain_since(Some(&previous), &deleted_too).unwrap();
        assert_eq!(drain.entries, 4, "{drain:?}");
        assert!(drain.draining);

        // Entries already held back at the previous pass are not new.
        let drain = trash_drain_since(Some(&held), &held).unwrap();
        assert_eq!((drain.entries, drain.held_back), (0, 0));
    }

    /// Confirmed entries whose identity changed neither count as deletable
    /// nor raise the stalled-deleter advice once everything else drained:
    /// the receipt names them as the cause and stays silent.
    #[test]
    fn identity_changed_entries_are_explained_but_never_advised() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let kept = crate::CheckoutTrashBacklog {
            recorded: 18,
            identity_changed: 18,
            identity_changed_device: 18,
            ..crate::CheckoutTrashBacklog::default()
        };
        let mut finished = empty_pass(crate::ArtifactSweepLane::Ran, kept);
        finished.complete = true;
        for pass in 0..=UNRECLAIMED_PASSES_ADVICE_THRESHOLD {
            write_last_pass(&plan, 1 + pass, Some(&finished), None).unwrap();
        }
        let receipt = read_last_pass(&plan).unwrap().unwrap();
        assert!(receipt.passes_without_reclaim > UNRECLAIMED_PASSES_ADVICE_THRESHOLD);
        assert_eq!(
            receipt.reclaimed_nothing_because.as_deref(),
            Some("trash_entries_identity_changed")
        );
        assert_eq!(receipt.trash.unwrap().identity_changed, 18);
        assert!(reclaim_advice(&plan.home, &plan.repository_uuid).is_none());
    }

    /// Every pass says per lane what it found, and a pass that reclaimed
    /// nothing records the one cause; the run of empty passes survives a
    /// failed pass and ends at the first pass that reclaims.
    #[test]
    fn receipts_explain_empty_passes_and_count_them() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let yielded = empty_pass(
            crate::ArtifactSweepLane::GcLockHeld,
            crate::CheckoutTrashBacklog::default(),
        );
        write_last_pass(&plan, 10, Some(&yielded), None).unwrap();
        let receipt = read_last_pass(&plan).unwrap().unwrap();
        assert_eq!(receipt.artifact_lane.as_deref(), Some("gc_lock_held"));
        assert_eq!(
            receipt.reclaimed_nothing_because.as_deref(),
            Some("artifact_lane_yielded_to_worker")
        );
        assert_eq!(receipt.passes_without_reclaim, 1);
        assert_eq!(receipt.unreclaimed_since_unix_ms, Some(10));

        let stuck = crate::CheckoutTrashBacklog {
            recorded: 200,
            pending: 180,
            stale_pending: 180,
            deletable: 20,
            ..crate::CheckoutTrashBacklog::default()
        };
        write_last_pass(
            &plan,
            20,
            Some(&empty_pass(crate::ArtifactSweepLane::Ran, stuck)),
            None,
        )
        .unwrap();
        write_last_pass(&plan, 30, None, Some("broker open failed")).unwrap();
        let receipt = read_last_pass(&plan).unwrap().unwrap();
        assert_eq!(receipt.outcome, "error");
        assert_eq!(
            receipt.passes_without_reclaim, 2,
            "a failed pass keeps the run"
        );
        assert_eq!(receipt.unreclaimed_since_unix_ms, Some(10));

        let confirming = crate::CheckoutTrashBacklog {
            confirmed_this_pass: 180,
            deletable: 200,
            ..crate::CheckoutTrashBacklog::default()
        };
        write_last_pass(
            &plan,
            40,
            Some(&empty_pass(crate::ArtifactSweepLane::Ran, confirming)),
            None,
        )
        .unwrap();
        let receipt = read_last_pass(&plan).unwrap().unwrap();
        assert_eq!(receipt.passes_without_reclaim, 0);
        assert_eq!(receipt.unreclaimed_since_unix_ms, None);
        assert_eq!(receipt.reclaimed_nothing_because, None);
        assert_eq!(receipt.trash.unwrap().confirmed_this_pass, 180);
        assert!(fs::metadata(&plan.receipt_path).unwrap().len() <= MAX_RECEIPT_BYTES as u64);
    }

    fn trash(deletable: usize, oldest: Option<i64>, stale: usize) -> crate::CheckoutTrashBacklog {
        crate::CheckoutTrashBacklog {
            recorded: deletable + stale,
            deletable,
            pending: stale,
            stale_pending: stale,
            oldest_deletable_created_ms: oldest,
            ..crate::CheckoutTrashBacklog::default()
        }
    }

    /// `broker status` speaks only after the threshold of empty passes, for
    /// a deleter that is not getting through confirmed trash; that cause
    /// wins over a lane that yielded to the detached worker.
    #[test]
    fn status_advice_names_a_stalled_deleter_after_two_hours_of_empty_passes() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let stalled = trash(20, Some(5), 0);
        for pass in 0..UNRECLAIMED_PASSES_ADVICE_THRESHOLD {
            assert!(
                reclaim_advice(&plan.home, &plan.repository_uuid).is_none(),
                "no advice before the threshold (pass {pass})"
            );
            write_last_pass(
                &plan,
                100 + pass,
                Some(&empty_pass(crate::ArtifactSweepLane::GcLockHeld, stalled)),
                None,
            )
            .unwrap();
        }
        let receipt = read_last_pass(&plan).unwrap().unwrap();
        assert_eq!(
            receipt.reclaimed_nothing_because.as_deref(),
            Some("trash_not_draining")
        );
        assert_eq!(receipt.trash_entries_drained, Some(0));
        let advice = reclaim_advice(&plan.home, &plan.repository_uuid).expect("advice");
        assert_eq!(advice.id, "storage.gc-service-reclaiming-nothing");
        assert_eq!(advice.commands[0], "aethyme broker gc trash drain --json");
    }

    /// Trash rows the proofs keep (identity changed, session in use) are
    /// never advised, and do not hide the lane that actually blocked.
    #[test]
    fn rows_the_proofs_keep_are_explained_but_never_advised() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let kept = trash(0, None, 30);
        write_last_pass(
            &plan,
            1,
            Some(&empty_pass(crate::ArtifactSweepLane::GcLockHeld, kept)),
            None,
        )
        .unwrap();
        assert_eq!(
            read_last_pass(&plan)
                .unwrap()
                .unwrap()
                .reclaimed_nothing_because
                .as_deref(),
            Some("artifact_lane_yielded_to_worker")
        );
        let mut finished = empty_pass(crate::ArtifactSweepLane::Ran, kept);
        finished.complete = true;
        for pass in 0..UNRECLAIMED_PASSES_ADVICE_THRESHOLD {
            write_last_pass(&plan, 2 + pass, Some(&finished), None).unwrap();
        }
        let receipt = read_last_pass(&plan).unwrap().unwrap();
        assert!(receipt.passes_without_reclaim > UNRECLAIMED_PASSES_ADVICE_THRESHOLD);
        assert_eq!(
            receipt.reclaimed_nothing_because.as_deref(),
            Some("trash_entries_kept_by_proofs")
        );
        assert!(reclaim_advice(&plan.home, &plan.repository_uuid).is_none());
    }

    /// While the paced deleter works through the backlog (fewer deletable
    /// entries, or the oldest one moving forward), passes count as
    /// reclaiming and nothing says the deleter is stalled.
    #[test]
    fn a_shrinking_backlog_counts_as_reclaim() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let mut deletable = 200;
        write_last_pass(
            &plan,
            1,
            Some(&empty_pass(
                crate::ArtifactSweepLane::GcLockHeld,
                trash(deletable, Some(10), 0),
            )),
            None,
        )
        .unwrap();
        for pass in 0..UNRECLAIMED_PASSES_ADVICE_THRESHOLD {
            deletable -= 10;
            write_last_pass(
                &plan,
                2 + pass,
                Some(&empty_pass(
                    crate::ArtifactSweepLane::GcLockHeld,
                    trash(deletable, Some(10), 0),
                )),
                None,
            )
            .unwrap();
            let receipt = read_last_pass(&plan).unwrap().unwrap();
            assert_eq!(receipt.passes_without_reclaim, 0, "pass {pass}");
            assert_eq!(receipt.trash_entries_drained, Some(10));
            assert_eq!(receipt.reclaimed_nothing_because, None);
        }
        // Same count, but the oldest entry moved forward: still draining.
        write_last_pass(
            &plan,
            50,
            Some(&empty_pass(
                crate::ArtifactSweepLane::GcLockHeld,
                trash(deletable, Some(11), 0),
            )),
            None,
        )
        .unwrap();
        assert_eq!(
            read_last_pass(&plan)
                .unwrap()
                .unwrap()
                .passes_without_reclaim,
            0
        );
        assert!(reclaim_advice(&plan.home, &plan.repository_uuid).is_none());
    }

    /// A receipt an older binary wrote, without the per-lane fields, still
    /// reads.
    #[test]
    fn receipts_without_per_lane_fields_still_read() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let old = json!({
            "schema": RECEIPT_SCHEMA,
            "repository_uuid": plan.repository_uuid,
            "generation": plan.generation,
            "started_at_unix_ms": 1,
            "finished_at_unix_ms": 2,
            "outcome": "sweep_paused",
            "directories_reclaimed": 0,
            "budget_ms": 5000,
            "disk_pressured": false,
            "legacy_unscoped_state": "unknown_pending",
            "error": null
        });
        fs::write(&plan.receipt_path, serde_json::to_vec(&old).unwrap()).unwrap();
        fs::set_permissions(&plan.receipt_path, fs::Permissions::from_mode(0o600)).unwrap();
        let receipt = read_last_pass(&plan).unwrap().unwrap();
        assert_eq!(receipt.passes_without_reclaim, 0);
        assert!(receipt.trash.is_none());
        write_last_pass(
            &plan,
            3,
            Some(&empty_pass(
                crate::ArtifactSweepLane::GcLockHeld,
                crate::CheckoutTrashBacklog::default(),
            )),
            None,
        )
        .unwrap();
        assert_eq!(
            read_last_pass(&plan)
                .unwrap()
                .unwrap()
                .passes_without_reclaim,
            1
        );
    }

    use crate::scheduled_service::launchd::LabelState;

    /// Leave an older installation's record in place of `plan`'s: a plan
    /// file naming another digest and a runtime at `router`, and, when
    /// `unit` is set, a unit file with other content.
    fn stale_record(plan: &Plan, router: &Path, unit: bool) {
        install_fixture(plan);
        let old = json!({
            "schema": PLAN_SCHEMA,
            "digest": "0".repeat(64),
            "runtime_version": "0.8.65",
            "router": router,
        });
        let path = plan.directory.join("plan.json");
        fs::remove_file(&path).unwrap();
        fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::remove_file(&plan.unit_path).unwrap();
        if unit {
            fs::write(&plan.unit_path, b"<plist>old</plist>").unwrap();
            fs::set_permissions(&plan.unit_path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn archived_plans(plan: &Plan) -> Vec<PathBuf> {
        fs::read_dir(plan.directory.join(RECORD_ARCHIVE_DIRECTORY))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path().join("plan.json"))
                    .filter(|path| path.is_file())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn status_reports_a_record_whose_unit_and_runtime_are_gone() {
        let (temp, plan) = fixture();
        stale_record(&plan, &temp.path().join("gone/aethyme"), false);
        let result =
            control_dead_record(&plan.home, &plan.repository_uuid, "status", None, |_, _| {
                Ok(LabelState::NotLoaded)
            })
            .unwrap()
            .unwrap();
        assert_eq!(result["installation_state"], "stale_record");
        assert_eq!(result["record"]["unit_file"], "missing");
        assert_eq!(result["record"]["recorded_runtime_present"], false);
        assert_eq!(result["record"]["recorded_runtime_version"], "0.8.65");
        assert_eq!(result["record"]["scheduler_state"], "not_loaded");
        assert_eq!(
            result["record"]["replaceable_because"],
            "unit_file_missing_and_label_not_loaded"
        );
        // An installation with its unit is not a stale record.
        let (_other, installed) = fixture();
        install_fixture(&installed);
        assert!(
            control_dead_record(
                &installed.home,
                &installed.repository_uuid,
                "status",
                None,
                |_, _| Ok(LabelState::NotLoaded),
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn install_replaces_only_a_provably_dead_record_and_archives_it() {
        // Unit gone, label unknown: no proof, the record stays.
        let (temp, plan) = fixture();
        stale_record(&plan, &temp.path().join("gone/aethyme"), false);
        assert!(install_replacing_dead_record(&plan, |_, _| Ok(LabelState::Unknown)).is_err());
        assert!(archived_plans(&plan).is_empty());
        // Unit gone, label not loaded: archived, then the new plan installed.
        let (created, archive) =
            install_replacing_dead_record(&plan, |_, _| Ok(LabelState::NotLoaded)).unwrap();
        assert!(archive.unwrap().join("plan.json").is_file());
        assert_eq!(archived_plans(&plan).len(), 1);
        assert!(created.contains(&plan.unit_path), "{created:?}");
        assert_eq!(
            fs::read(plan.directory.join("plan.json")).unwrap(),
            serde_json::to_vec(&plan).unwrap()
        );

        // A loaded, different service is never replaced.
        let (temp, plan) = fixture();
        stale_record(&plan, &temp.path().join("gone/aethyme"), true);
        assert!(install_replacing_dead_record(&plan, |_, _| Ok(LabelState::Loaded)).is_err());
        assert_eq!(fs::read(&plan.unit_path).unwrap(), b"<plist>old</plist>");

        // Its unit in place but its runtime gone, and not loaded: the old
        // unit leaves LaunchAgents for the archive and the new one goes in.
        let (created, archive) =
            install_replacing_dead_record(&plan, |_, _| Ok(LabelState::NotLoaded)).unwrap();
        assert!(archive.unwrap().join("unit.plist").is_file());
        assert_eq!(fs::read(&plan.unit_path).unwrap(), plan.unit.as_bytes());
        assert!(created.contains(&plan.unit_path), "{created:?}");

        // Its unit and runtime in place, not loaded: not proven dead.
        let (_temp, plan) = fixture();
        stale_record(&plan, &plan.router, true);
        assert!(install_replacing_dead_record(&plan, |_, _| Ok(LabelState::NotLoaded)).is_err());
    }

    #[test]
    fn uninstall_finishes_removing_a_dead_record() {
        let (temp, plan) = fixture();
        stale_record(&plan, &temp.path().join("gone/aethyme"), false);
        let old_digest = "0".repeat(64);
        // The exact recorded digest is required.
        assert!(
            control_dead_record(
                &plan.home,
                &plan.repository_uuid,
                "uninstall",
                Some(&plan.digest),
                |_, _| Ok(LabelState::NotLoaded),
            )
            .is_err()
        );
        // Launchd must confirm the label is not loaded.
        assert!(
            control_dead_record(
                &plan.home,
                &plan.repository_uuid,
                "uninstall",
                Some(&old_digest),
                |_, _| Ok(LabelState::Unknown),
            )
            .is_err()
        );
        assert!(plan.directory.join("plan.json").is_file());
        let result = control_dead_record(
            &plan.home,
            &plan.repository_uuid,
            "uninstall",
            Some(&old_digest),
            |_, _| Ok(LabelState::NotLoaded),
        )
        .unwrap()
        .unwrap();
        assert_eq!(result["installed_files_removed"], true);
        assert!(!plan.directory.join("plan.json").exists());
        assert_eq!(archived_plans(&plan).len(), 1);
    }

    #[test]
    fn plan_reports_a_replaceable_record() {
        let (temp, plan) = fixture();
        assert!(
            existing_installation_with(&plan, |_, _| Ok(LabelState::NotLoaded))
                .unwrap()
                .is_none()
        );
        stale_record(&plan, &temp.path().join("gone/aethyme"), false);
        let existing = existing_installation_with(&plan, |_, _| Ok(LabelState::NotLoaded))
            .unwrap()
            .unwrap();
        assert_eq!(existing["replaceable"], true);
        assert_eq!(existing["recorded_plan_digest"], "0".repeat(64));
    }

    #[test]
    fn label_state_maps_print_exits_and_needs_both_domains_for_not_loaded() {
        use crate::scheduled_service::launchd::{combine_domains, print_exit_state};
        assert_eq!(print_exit_state(Some(0)), LabelState::Loaded);
        assert_eq!(print_exit_state(Some(113)), LabelState::NotLoaded);
        assert_eq!(print_exit_state(Some(1)), LabelState::Unknown);
        assert_eq!(print_exit_state(Some(5)), LabelState::Unknown);
        assert_eq!(print_exit_state(None), LabelState::Unknown);
        let states = [
            LabelState::Loaded,
            LabelState::NotLoaded,
            LabelState::Unknown,
        ];
        for gui in states {
            for user in states {
                let expected = if gui == LabelState::Loaded || user == LabelState::Loaded {
                    LabelState::Loaded
                } else if gui == LabelState::NotLoaded && user == LabelState::NotLoaded {
                    LabelState::NotLoaded
                } else {
                    LabelState::Unknown
                };
                assert_eq!(combine_domains(gui, user), expected, "{gui:?} {user:?}");
            }
        }
    }

    /// The wedge a crash between the unit and plan moves used to leave: a
    /// unit with no plan. Status reports it, install replaces it, and
    /// uninstall removes it with the unit's digest, once launchd says the
    /// label is not loaded.
    #[test]
    fn a_unit_without_its_plan_is_a_recoverable_stale_record() {
        let (_temp, plan) = fixture();
        stale_record(&plan, &plan.router, true);
        fs::remove_file(plan.directory.join("plan.json")).unwrap();
        let result =
            control_dead_record(&plan.home, &plan.repository_uuid, "status", None, |_, _| {
                Ok(LabelState::NotLoaded)
            })
            .unwrap()
            .unwrap();
        assert_eq!(result["installation_state"], "stale_record");
        assert_eq!(result["record"]["plan_file"], "missing");
        assert_eq!(
            result["record"]["replaceable_because"],
            "plan_missing_and_label_not_loaded"
        );
        let unit_digest = digest(b"<plist>old</plist>");
        assert_eq!(result["record"]["recorded_unit_digest"], unit_digest);
        // Unknown: nothing moves.
        assert!(
            control_dead_record(
                &plan.home,
                &plan.repository_uuid,
                "uninstall",
                Some(&unit_digest),
                |_, _| Ok(LabelState::Unknown),
            )
            .is_err()
        );
        assert!(plan.unit_path.is_file());
        // Install replaces it once launchd confirms the label is unloaded.
        let (created, archive) =
            install_replacing_dead_record(&plan, |_, _| Ok(LabelState::NotLoaded)).unwrap();
        assert!(archive.unwrap().join("unit.plist").is_file());
        assert!(created.contains(&plan.unit_path), "{created:?}");

        // Uninstall finishes removing the same wedge, confirmed by unit digest.
        let (_temp, plan) = fixture();
        stale_record(&plan, &plan.router, true);
        fs::remove_file(plan.directory.join("plan.json")).unwrap();
        assert!(
            control_dead_record(
                &plan.home,
                &plan.repository_uuid,
                "uninstall",
                Some(&"0".repeat(64)),
                |_, _| Ok(LabelState::NotLoaded),
            )
            .is_err()
        );
        let result = control_dead_record(
            &plan.home,
            &plan.repository_uuid,
            "uninstall",
            Some(&unit_digest),
            |_, _| Ok(LabelState::NotLoaded),
        )
        .unwrap()
        .unwrap();
        assert_eq!(result["installed_files_removed"], true);
        assert!(!plan.unit_path.exists());
    }

    #[test]
    fn install_decides_under_the_installer_claim() {
        let (temp, plan) = fixture();
        stale_record(&plan, &temp.path().join("gone/aethyme"), false);
        let held =
            crate::pr_watch_service_install::claim_installer(&plan.directory, plan.uid).unwrap();
        let mut asked = false;
        let error = install_replacing_dead_record(&plan, |_, _| {
            asked = true;
            Ok(LabelState::NotLoaded)
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock, "{error}");
        assert!(!asked, "launchd must not be asked before the claim is held");
        assert!(archived_plans(&plan).is_empty());
        drop(held);
        install_replacing_dead_record(&plan, |_, _| Ok(LabelState::NotLoaded)).unwrap();
        assert_eq!(archived_plans(&plan).len(), 1);
    }

    #[test]
    fn archives_are_pruned_to_the_newest_five_numeric_names() {
        let (temp, plan) = fixture();
        let archives = plan.directory.join(RECORD_ARCHIVE_DIRECTORY);
        install_fixture(&plan);
        fs::create_dir(&archives).unwrap();
        for stamp in [10_u64, 20, 30, 40, 50] {
            fs::create_dir(archives.join(stamp.to_string())).unwrap();
        }
        fs::create_dir(archives.join("notes")).unwrap();
        stale_record(&plan, &temp.path().join("gone/aethyme"), false);
        let record = read_record(&plan.home, &plan.repository_uuid, plan.uid).unwrap();
        let newest = archive_record(&record).unwrap();
        let mut names: Vec<String> = fs::read_dir(&archives)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let newest = newest.file_name().unwrap().to_string_lossy().into_owned();
        let mut expected = vec!["20", "30", "40", "50", "notes", newest.as_str()];
        expected.sort();
        assert_eq!(names, expected);
    }

    #[test]
    fn archiving_refuses_a_symlinked_archive_directory_or_unit() {
        use std::os::unix::fs::symlink;
        let (temp, plan) = fixture();
        stale_record(&plan, &temp.path().join("gone/aethyme"), false);
        let elsewhere = temp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        symlink(&elsewhere, plan.directory.join(RECORD_ARCHIVE_DIRECTORY)).unwrap();
        let record = read_record(&plan.home, &plan.repository_uuid, plan.uid).unwrap();
        assert!(archive_record(&record).is_err());
        assert!(plan.directory.join("plan.json").is_file());
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);

        let (temp, plan) = fixture();
        stale_record(&plan, &temp.path().join("gone/aethyme"), false);
        let target = temp.path().join("foreign.plist");
        fs::write(&target, b"foreign").unwrap();
        symlink(&target, &plan.unit_path).unwrap();
        let record = read_record(&plan.home, &plan.repository_uuid, plan.uid).unwrap();
        assert!(record.unit_present);
        assert!(archive_record(&record).is_err());
        assert!(plan.directory.join("plan.json").is_file());
        assert_eq!(fs::read(&target).unwrap(), b"foreign");
    }

    #[test]
    fn a_cross_volume_unit_move_copies_syncs_then_removes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("unit.plist");
        let destination = temp.path().join("archived.plist");
        fs::write(&source, b"unit bytes").unwrap();
        move_file_with(&source, &destination, |_, _| {
            Err(io::Error::from_raw_os_error(libc::EXDEV))
        })
        .unwrap();
        assert!(!source.exists());
        assert_eq!(fs::read(&destination).unwrap(), b"unit bytes");
        // Any other rename failure leaves the source where it was.
        fs::write(&source, b"unit bytes").unwrap();
        assert!(
            move_file_with(&source, &temp.path().join("other"), |_, _| {
                Err(io::Error::from_raw_os_error(libc::EACCES))
            })
            .is_err()
        );
        assert!(source.is_file());
    }

    /// A newer runtime pair beside the fixture's, as an update installs it.
    fn newer_runtime(plan: &Plan) -> RuntimePair {
        let version_directory = plan.managed_root.join("versions/v0.8.51-test");
        fs::create_dir_all(&version_directory).unwrap();
        let router = version_directory.join("aethyme");
        let engine = version_directory.join("aethyme-engine-cli");
        fs::write(&router, b"newer router fixture").unwrap();
        fs::write(&engine, b"newer engine fixture").unwrap();
        fs::set_permissions(&router, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&engine, fs::Permissions::from_mode(0o700)).unwrap();
        RuntimePair {
            managed_root: plan.managed_root.clone(),
            version_directory,
            runtime_version: "0.8.51".into(),
            runtime_build_identity: "0.8.51 (build_revision=fixture)".into(),
            router_sha256: binary_digest(&router).unwrap(),
            engine_sha256: binary_digest(&engine).unwrap(),
            router,
            engine,
        }
    }

    fn refreshed_plan(plan: &Plan, runtime: &RuntimePair) -> Plan {
        Plan::from_runtime(
            &plan.repository,
            &plan.repository_uuid,
            &plan.home,
            plan.uid,
            runtime,
        )
        .unwrap()
    }

    fn assert_unchanged(plan: &Plan) {
        assert_eq!(
            fs::read(plan.directory.join("plan.json")).unwrap(),
            serde_json::to_vec(plan).unwrap()
        );
        assert_eq!(fs::read(&plan.unit_path).unwrap(), plan.unit.as_bytes());
        assert!(archived_plans(plan).is_empty());
    }

    #[test]
    fn refresh_of_a_loaded_service_republishes_and_rebootstraps_it() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let runtime = newer_runtime(&plan);
        let fresh = refreshed_plan(&plan, &runtime);
        let mut calls: Vec<Vec<String>> = Vec::new();
        let outcome = refresh_record_with(
            &plan.home,
            &plan.repository_uuid,
            plan.uid,
            Ok(&runtime),
            |_, label| {
                assert_eq!(label, plan.label);
                Ok(LabelState::Loaded)
            },
            |args| {
                calls.push(args.to_vec());
                Ok(Observation::Success)
            },
        );
        assert_eq!(outcome.outcome, "refreshed_and_reloaded", "{outcome:?}");
        assert_eq!(outcome.scheduler_state, "loaded");
        assert!(!outcome.keep_previous_runtime);
        assert_eq!(outcome.plan_digest.as_deref(), Some(fresh.digest.as_str()));
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0],
            [
                "bootout".to_string(),
                format!("gui/{}/{}", plan.uid, plan.label)
            ]
        );
        assert_eq!(calls[1][0], "bootstrap");
        assert_eq!(calls[1][2], fresh.unit_path.to_string_lossy());
        assert_eq!(
            fs::read(plan.directory.join("plan.json")).unwrap(),
            serde_json::to_vec(&fresh).unwrap()
        );
        assert_eq!(fs::read(&plan.unit_path).unwrap(), fresh.unit.as_bytes());
        let archive = outcome.archived_to.clone().unwrap();
        assert_eq!(
            fs::read(archive.join("plan.json")).unwrap(),
            serde_json::to_vec(&plan).unwrap()
        );
        assert_eq!(
            fs::read(archive.join("unit.plist")).unwrap(),
            plan.unit.as_bytes()
        );
        let control = read_control(&fresh).unwrap().unwrap();
        assert_eq!(control.action, "refresh");
        assert_eq!(control.scheduler_state, "loaded");
        assert_eq!(control.operation_result, "acknowledged");
        let receipt =
            read_bounded_json(&plan.directory.join("last-refresh.json"), plan.uid).unwrap();
        assert_eq!(receipt["outcome"], "refreshed_and_reloaded");
        // The refreshed record loads and is current: a second refresh is a no-op.
        let again = refresh_record_with(
            &plan.home,
            &plan.repository_uuid,
            plan.uid,
            Ok(&runtime),
            |_, _| panic!("launchd is not asked about a current record"),
            |_| panic!("launchctl is not run for a current record"),
        );
        assert_eq!(again.outcome, "already_current");
    }

    #[test]
    fn refresh_of_an_unloaded_service_republishes_without_launchctl() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let runtime = newer_runtime(&plan);
        let fresh = refreshed_plan(&plan, &runtime);
        let outcome = refresh_record_with(
            &plan.home,
            &plan.repository_uuid,
            plan.uid,
            Ok(&runtime),
            |_, _| Ok(LabelState::NotLoaded),
            |_| panic!("an unloaded service is never bootstrapped by a refresh"),
        );
        assert_eq!(outcome.outcome, "refreshed_not_loaded", "{outcome:?}");
        assert!(!outcome.keep_previous_runtime);
        assert_eq!(fs::read(&plan.unit_path).unwrap(), fresh.unit.as_bytes());
        assert_eq!(archived_plans(&plan).len(), 1);
        assert!(read_control(&fresh).unwrap().is_none());
    }

    #[test]
    fn refresh_with_an_unknown_label_changes_nothing_and_keeps_the_old_bundle() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let runtime = newer_runtime(&plan);
        let report = refresh_for_update_with(
            &plan.home,
            plan.uid,
            &plan.managed_root,
            std::ffi::OsStr::new("v0.8.51-test"),
            || Ok(runtime.clone()),
            |_, _| Ok(LabelState::Unknown),
            |_| panic!("nothing is run without launchd's answer"),
        );
        assert_eq!(report.outcomes.len(), 1);
        assert_eq!(report.outcomes[0].outcome, "label_state_unknown");
        assert_eq!(
            report.protected_bundles,
            vec![std::ffi::OsString::from("v0.8.50-test")]
        );
        assert_unchanged(&plan);
        // The attempt is recorded beside the plan.
        let receipt =
            read_bounded_json(&plan.directory.join("last-refresh.json"), plan.uid).unwrap();
        assert_eq!(receipt["outcome"], "label_state_unknown");
    }

    #[test]
    fn update_refresh_skips_services_bound_to_the_bundle_it_selected() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        // Reinstalling the bundle the service already runs: nothing to do.
        let report = refresh_for_update_with(
            &plan.home,
            plan.uid,
            &plan.managed_root,
            std::ffi::OsStr::new("v0.8.50-test"),
            || panic!("no runtime is resolved when no service needs it"),
            |_, _| panic!("launchd is not asked"),
            |_| panic!("launchctl is not run"),
        );
        assert!(report.outcomes.is_empty());
        assert!(report.protected_bundles.is_empty());
        // Another installation's services are never touched.
        let report = refresh_for_update_with(
            &plan.home,
            plan.uid,
            &plan.home.join("other-install/.aethyme-managed"),
            std::ffi::OsStr::new("v0.8.51-test"),
            || panic!("no runtime is resolved for another installation"),
            |_, _| panic!("launchd is not asked"),
            |_| panic!("launchctl is not run"),
        );
        assert!(report.outcomes.is_empty());
        assert_unchanged(&plan);
    }

    #[test]
    fn a_refresh_failure_is_reported_and_never_fails_the_update() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let active = std::ffi::OsStr::new("v0.8.51-test");
        // The new pair cannot be verified and launchd says the job is loaded:
        // nothing changes, the error is reported, the old bundle is kept.
        let report = refresh_for_update_with(
            &plan.home,
            plan.uid,
            &plan.managed_root,
            active,
            || Err(io::Error::other("new runtime pair is not verifiable")),
            |_, _| Ok(LabelState::Loaded),
            |_| panic!("launchctl is not run when the refresh failed"),
        );
        let outcome = &report.outcomes[0];
        assert_eq!(outcome.outcome, "refresh_failed");
        assert!(
            outcome
                .error
                .as_deref()
                .is_some_and(|error| error.contains("not verifiable"))
        );
        assert_eq!(
            report.protected_bundles,
            vec![std::ffi::OsString::from("v0.8.50-test")]
        );
        assert_unchanged(&plan);
        // Not loaded: nothing can run the old bundle, so it is not kept.
        let report = refresh_for_update_with(
            &plan.home,
            plan.uid,
            &plan.managed_root,
            active,
            || Err(io::Error::other("new runtime pair is not verifiable")),
            |_, _| Ok(LabelState::NotLoaded),
            |_| panic!("launchctl is not run"),
        );
        assert_eq!(report.outcomes[0].outcome, "refresh_failed");
        assert!(report.protected_bundles.is_empty());
        // A launchctl that cannot be run is an error in the outcome too.
        let runtime = newer_runtime(&plan);
        let report = refresh_for_update_with(
            &plan.home,
            plan.uid,
            &plan.managed_root,
            active,
            || Ok(runtime.clone()),
            |_, _| Err(io::Error::other("launchctl unavailable")),
            |_| panic!("launchctl is not run"),
        );
        assert_eq!(report.outcomes[0].outcome, "label_state_unknown");
        assert!(report.outcomes[0].error.is_some());
        assert_eq!(
            report.protected_bundles,
            vec![std::ffi::OsString::from("v0.8.50-test")]
        );
        assert_unchanged(&plan);
    }

    #[test]
    fn a_service_mid_tick_keeps_its_bundle_and_is_left_alone() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let runtime = newer_runtime(&plan);
        // A running tick holds the installer claim for its whole pass.
        let tick =
            crate::pr_watch_service_install::claim_installer(&plan.directory, plan.uid).unwrap();
        let report = refresh_for_update_with(
            &plan.home,
            plan.uid,
            &plan.managed_root,
            std::ffi::OsStr::new("v0.8.51-test"),
            || Ok(runtime.clone()),
            |_, _| panic!("launchd is not asked without the claim"),
            |_| panic!("launchctl is not run without the claim"),
        );
        drop(tick);
        assert_eq!(report.outcomes[0].outcome, "refresh_failed");
        assert_eq!(
            report.protected_bundles,
            vec![std::ffi::OsString::from("v0.8.50-test")]
        );
        assert_unchanged(&plan);
    }

    #[test]
    fn a_failed_publish_puts_the_previous_record_back() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let runtime = newer_runtime(&plan);
        // A directory where the publisher's scratch file goes makes every
        // publish fail after the record was archived.
        fs::create_dir(plan.directory.join(".install.pending")).unwrap();
        let outcome = refresh_record_with(
            &plan.home,
            &plan.repository_uuid,
            plan.uid,
            Ok(&runtime),
            |_, _| Ok(LabelState::Loaded),
            |_| panic!("nothing is reloaded after a failed publish"),
        );
        assert_eq!(outcome.outcome, "refresh_failed_restored", "{outcome:?}");
        assert!(outcome.error.is_some());
        assert!(outcome.keep_previous_runtime, "the old job is still loaded");
        assert_unchanged(&plan);
        load(&plan.home, &plan.repository_uuid).unwrap();
    }

    #[test]
    fn an_unrestored_refresh_is_advised_and_repaired_by_refresh() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let runtime = newer_runtime(&plan);
        let fresh = refreshed_plan(&plan, &runtime);
        let mut out = RefreshOutcome::new(&plan.repository_uuid);
        {
            let claim = crate::pr_watch_service_install::claim_service_directory(
                &plan.home,
                &plan.directory,
                plan.uid,
            )
            .unwrap();
            let record = read_record(&plan.home, &plan.repository_uuid, plan.uid).unwrap();
            let archive = archive_record(&record).unwrap();
            fs::create_dir(plan.directory.join(".install.pending")).unwrap();
            let error = publish_or_restore(
                &fresh,
                &record,
                &archive,
                &claim,
                |_, _, _, _| Err(io::Error::other("injected restore failure")),
                &mut out,
            )
            .unwrap_err();
            assert!(error.to_string().contains("injected restore failure"));
            assert_eq!(out.outcome, "refresh_failed_unrestored");
            out.archived_to = Some(archive);
            write_refresh_receipt(&plan.home, &plan.repository_uuid, &out).unwrap();
        }
        assert!(!plan.directory.join("plan.json").exists());
        assert!(!plan.unit_path.exists());
        let advice = repair_advice(&plan.home, &plan.repository_uuid, plan.uid).unwrap();
        assert_eq!(advice.id, "storage.gc-service-record-interrupted");
        assert_eq!(advice.commands, vec![REFRESH_COMMAND.to_string()]);
        let status =
            control_dead_record(&plan.home, &plan.repository_uuid, "status", None, |_, _| {
                panic!("status of an interrupted refresh needs no launchd answer")
            })
            .unwrap()
            .unwrap();
        assert_eq!(status["installation_state"], "refresh_interrupted");
        assert_eq!(status["next_action"], REFRESH_COMMAND);
        // `refresh` puts the record back, then refreshes it.
        fs::remove_dir(plan.directory.join(".install.pending")).unwrap();
        let outcome = refresh_record_with(
            &plan.home,
            &plan.repository_uuid,
            plan.uid,
            Ok(&runtime),
            |_, _| Ok(LabelState::NotLoaded),
            |_| panic!("an unloaded service is not bootstrapped"),
        );
        assert_eq!(outcome.outcome, "refreshed_not_loaded", "{outcome:?}");
        assert_eq!(fs::read(&plan.unit_path).unwrap(), fresh.unit.as_bytes());
        assert!(repair_advice(&plan.home, &plan.repository_uuid, plan.uid).is_none());
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn restoring_keeps_a_newer_receipt_and_still_restores_the_unit() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        write_private(&plan.directory.join("last-error.json"), b"{\"older\":true}");
        let mut out = RefreshOutcome::new(&plan.repository_uuid);
        {
            let _claim = crate::pr_watch_service_install::claim_service_directory(
                &plan.home,
                &plan.directory,
                plan.uid,
            )
            .unwrap();
            let record = read_record(&plan.home, &plan.repository_uuid, plan.uid).unwrap();
            let archive = archive_record(&record).unwrap();
            assert!(archive.join("last-error.json").is_file());
            out.outcome = "refresh_failed_unrestored";
            out.archived_to = Some(archive);
            write_refresh_receipt(&plan.home, &plan.repository_uuid, &out).unwrap();
        }
        // The still-loaded job's tick fails without a plan and leaves a
        // newer error receipt, without the installer claim.
        write_private(&plan.directory.join("last-error.json"), b"{\"newer\":true}");
        let outcome = refresh_record_with(
            &plan.home,
            &plan.repository_uuid,
            plan.uid,
            Ok(&newer_runtime(&plan)),
            |_, _| Ok(LabelState::Unknown),
            |_| panic!("nothing is run without launchd's answer"),
        );
        assert_eq!(outcome.outcome, "label_state_unknown", "{outcome:?}");
        assert_eq!(
            fs::read(plan.directory.join("plan.json")).unwrap(),
            serde_json::to_vec(&plan).unwrap()
        );
        assert_eq!(fs::read(&plan.unit_path).unwrap(), plan.unit.as_bytes());
        assert_eq!(
            fs::read(plan.directory.join("last-error.json")).unwrap(),
            b"{\"newer\":true}"
        );
        let archive = out.archived_to.unwrap();
        assert_eq!(
            fs::read(archive.join("last-error.json")).unwrap(),
            b"{\"older\":true}"
        );
        load(&plan.home, &plan.repository_uuid).unwrap();
    }

    #[test]
    fn a_plan_without_its_unit_is_advised() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        fs::remove_file(&plan.unit_path).unwrap();
        let advice = repair_advice(&plan.home, &plan.repository_uuid, plan.uid).unwrap();
        assert_eq!(advice.id, "storage.gc-service-unit-missing");
    }

    fn unconfirmed_control(plan: &Plan) {
        write_control(
            plan,
            &ControlReceipt {
                schema: CONTROL_SCHEMA.to_string(),
                plan_digest: plan.digest.clone(),
                generation: plan.generation.clone(),
                action: "refresh".to_string(),
                scheduler_state: "unknown".to_string(),
                operation_result: "refused".to_string(),
                observed_at_unix_ms: 1,
            },
        )
        .unwrap();
    }

    #[test]
    fn an_unconfirmed_load_is_advised_and_retried_by_status() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        assert!(repair_advice(&plan.home, &plan.repository_uuid, plan.uid).is_none());
        unconfirmed_control(&plan);
        let advice = repair_advice(&plan.home, &plan.repository_uuid, plan.uid).unwrap();
        assert_eq!(advice.id, "storage.gc-service-load-unconfirmed");
        assert!(advice.commands.contains(&format!(
            "aethyme broker gc service activate --confirm {}",
            plan.digest
        )));
        // Another repository's marker: no retry, no launchd call.
        assert!(
            retry_unconfirmed_load_with(
                &plan.home,
                &plan.repository_uuid,
                "00000000-0000-4000-8000-000000000000",
                &plan.repository,
                |_, _| panic!("launchd is not asked"),
                |_| panic!("launchctl is not run"),
            )
            .unwrap()
            .is_none()
        );
        // Unknown: nothing is run and the advice stays.
        let retry = retry_unconfirmed_load_with(
            &plan.home,
            &plan.repository_uuid,
            &plan.repository_uuid,
            &plan.repository,
            |_, _| Ok(LabelState::Unknown),
            |_| panic!("launchctl is not run without launchd's answer"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(retry["retried"], false);
        assert!(repair_advice(&plan.home, &plan.repository_uuid, plan.uid).is_some());
        // Not loaded: bootstrapped once, and the acknowledgement clears it.
        let mut calls = Vec::new();
        let retry = retry_unconfirmed_load_with(
            &plan.home,
            &plan.repository_uuid,
            &plan.repository_uuid,
            &plan.repository,
            |_, _| Ok(LabelState::NotLoaded),
            |args| {
                calls.push(args.to_vec());
                Ok(Observation::Success)
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(retry["retried"], true);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0][0], "bootstrap");
        assert_eq!(
            read_control(&plan).unwrap().unwrap().operation_result,
            "acknowledged"
        );
        assert!(repair_advice(&plan.home, &plan.repository_uuid, plan.uid).is_none());
        // Loaded after all: the observation clears the advice without a call.
        unconfirmed_control(&plan);
        retry_unconfirmed_load_with(
            &plan.home,
            &plan.repository_uuid,
            &plan.repository_uuid,
            &plan.repository,
            |_, _| Ok(LabelState::Loaded),
            |_| panic!("a loaded job is not bootstrapped again"),
        )
        .unwrap()
        .unwrap();
        assert!(repair_advice(&plan.home, &plan.repository_uuid, plan.uid).is_none());
    }

    #[test]
    fn the_update_walk_stops_at_its_budget_and_keeps_the_bundle() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let report = refresh_for_update_within(
            &plan.home,
            plan.uid,
            &plan.managed_root,
            std::ffi::OsStr::new("v0.8.51-test"),
            std::time::Instant::now(),
            || panic!("no runtime is resolved past the budget"),
            |_, _| panic!("launchd is not asked past the budget"),
            |_| panic!("launchctl is not run past the budget"),
        );
        let outcome = &report.outcomes[0];
        assert_eq!(outcome.outcome, "label_state_unknown");
        assert_eq!(outcome.kept_bundle.as_deref(), Some("v0.8.50-test"));
        assert_eq!(
            report.protected_bundles,
            vec![std::ffi::OsString::from("v0.8.50-test")]
        );
        assert_unchanged(&plan);
        let receipt =
            read_bounded_json(&plan.directory.join("last-refresh.json"), plan.uid).unwrap();
        assert_eq!(receipt["outcome"], "label_state_unknown");
    }

    #[test]
    fn a_refused_bootout_keeps_the_old_bundle() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let runtime = newer_runtime(&plan);
        let mut calls = 0;
        let outcome = refresh_record_with(
            &plan.home,
            &plan.repository_uuid,
            plan.uid,
            Ok(&runtime),
            |_, _| Ok(LabelState::Loaded),
            |_| {
                calls += 1;
                Ok(Observation::Refused)
            },
        );
        assert_eq!(calls, 1, "no bootstrap after a refused bootout");
        assert_eq!(outcome.outcome, "refreshed_reload_unconfirmed");
        assert!(outcome.keep_previous_runtime);
    }

    #[test]
    fn a_unit_that_differs_from_its_plan_is_never_repaired_automatically() {
        let (temp, plan) = fixture();
        install_fixture(&plan);
        let runtime = newer_runtime(&plan);
        let missing = temp.path().join("gone");
        // Bind the record to a runtime that is gone, as two updates leave it.
        let mut stale = RuntimePair {
            managed_root: plan.managed_root.clone(),
            version_directory: plan.managed_root.join("versions/v0.8.40-gone"),
            runtime_version: plan.runtime_version.clone(),
            runtime_build_identity: plan.runtime_build_identity.clone(),
            router: PathBuf::new(),
            engine: PathBuf::new(),
            router_sha256: plan.router_sha256.clone(),
            engine_sha256: plan.engine_sha256.clone(),
        };
        stale.router = stale.version_directory.join("aethyme");
        stale.engine = stale.version_directory.join("aethyme-engine-cli");
        let stale_plan = refreshed_plan(&plan, &stale);
        fs::remove_file(plan.directory.join("plan.json")).unwrap();
        fs::remove_file(&plan.unit_path).unwrap();
        install_fixture(&stale_plan);
        select_current(&plan, "v0.8.51-test");
        automatic_refresh_proof(
            &plan.home,
            &plan.repository_uuid,
            &plan.repository_uuid,
            &plan.repository,
            &runtime.router,
        )
        .unwrap();
        // Only the current selection may rebind it by itself.
        assert_eq!(
            automatic_refresh_proof(
                &plan.home,
                &plan.repository_uuid,
                &plan.repository_uuid,
                &plan.repository,
                &plan.router,
            ),
            Err("running_runtime_is_not_the_current_selection")
        );
        // Another repository's marker, or another repository: advice only.
        assert!(
            automatic_refresh_proof(
                &plan.home,
                &plan.repository_uuid,
                "00000000-0000-4000-8000-000000000000",
                &plan.repository,
                &runtime.router,
            )
            .is_err()
        );
        assert!(
            automatic_refresh_proof(
                &plan.home,
                &plan.repository_uuid,
                &plan.repository_uuid,
                &missing,
                &runtime.router,
            )
            .is_err()
        );
        assert!(
            repair_advice(&plan.home, &plan.repository_uuid, plan.uid)
                .unwrap()
                .evidence
                .iter()
                .any(|line| line.contains("no longer installed"))
        );
        // The unit no longer matches the plan's rendered unit.
        fs::remove_file(&stale_plan.unit_path).unwrap();
        fs::write(&stale_plan.unit_path, b"<plist>edited</plist>").unwrap();
        fs::set_permissions(&stale_plan.unit_path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            automatic_refresh_proof(
                &plan.home,
                &plan.repository_uuid,
                &plan.repository_uuid,
                &plan.repository,
                &runtime.router,
            ),
            Err("recorded_plan_or_unit_not_verified")
        );
        let outcome = refresh_record_with(
            &plan.home,
            &plan.repository_uuid,
            plan.uid,
            Ok(&runtime),
            |_, _| panic!("launchd is not asked about an unverified record"),
            |_| panic!("launchctl is not run for an unverified record"),
        );
        assert_eq!(outcome.outcome, "not_verifiable");
        assert_eq!(
            fs::read(&stale_plan.unit_path).unwrap(),
            b"<plist>edited</plist>"
        );
        assert!(archived_plans(&plan).is_empty());
    }

    /// Point the fixture installation's `current` selector at `bundle`.
    fn select_current(plan: &Plan, bundle: &str) {
        let link = plan.managed_root.join("current");
        match fs::symlink_metadata(&link) {
            Ok(_) => fs::remove_file(&link).unwrap(),
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::NotFound),
        }
        std::os::unix::fs::symlink(PathBuf::from("versions").join(bundle), &link).unwrap();
    }

    #[test]
    fn update_refresh_rebinds_a_service_bound_to_the_bundle_kept_as_previous() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let runtime = newer_runtime(&plan);
        let fresh = refreshed_plan(&plan, &runtime);
        // The update selected v0.8.51 as `current` and kept v0.8.50 as
        // `previous`: the bundle stays, but ticks refuse a runtime that is
        // not the current selection, so the service is rebound now.
        let report = refresh_for_update_with(
            &plan.home,
            plan.uid,
            &plan.managed_root,
            std::ffi::OsStr::new("v0.8.51-test"),
            || Ok(runtime.clone()),
            |_, _| Ok(LabelState::NotLoaded),
            |_| panic!("an unloaded service is never bootstrapped by a refresh"),
        );
        assert_eq!(report.outcomes.len(), 1);
        assert_eq!(report.outcomes[0].outcome, "refreshed_not_loaded");
        assert!(report.protected_bundles.is_empty());
        assert_eq!(
            fs::read(plan.directory.join("plan.json")).unwrap(),
            serde_json::to_vec(&fresh).unwrap()
        );
        assert!(
            plan.router.exists(),
            "the previous bundle itself is untouched"
        );
    }

    #[test]
    fn update_refresh_candidates_are_every_other_or_missing_bundle() {
        let a = std::ffi::OsStr::new("v0.8.51-a");
        let b = std::ffi::OsStr::new("v0.8.50-b");
        assert!(!needs_update_refresh(a, a, true));
        assert!(needs_update_refresh(a, a, false));
        assert!(needs_update_refresh(b, a, true));
        assert!(needs_update_refresh(b, a, false));
    }

    #[test]
    fn runtime_binding_follows_the_current_selector() {
        let (_temp, plan) = fixture();
        // No selector: nothing is proven either way.
        assert_eq!(
            runtime_binding(&plan.router, &plan.managed_root),
            RuntimeBinding::Unknown
        );
        select_current(&plan, "v0.8.50-test");
        assert_eq!(
            runtime_binding(&plan.router, &plan.managed_root),
            RuntimeBinding::Current
        );
        assert!(running_is_current_selection(
            &plan.router,
            &plan.managed_root
        ));
        let runtime = newer_runtime(&plan);
        select_current(&plan, "v0.8.51-test");
        assert_eq!(
            runtime_binding(&plan.router, &plan.managed_root),
            RuntimeBinding::Stale
        );
        assert!(!running_is_current_selection(
            &plan.router,
            &plan.managed_root
        ));
        assert!(running_is_current_selection(
            &runtime.router,
            &plan.managed_root
        ));
        fs::remove_file(&plan.router).unwrap();
        assert_eq!(
            runtime_binding(&plan.router, &plan.managed_root),
            RuntimeBinding::Gone
        );
        assert!(RuntimeBinding::Gone.needs_refresh());
        assert!(RuntimeBinding::Stale.needs_refresh());
        assert!(!RuntimeBinding::Current.needs_refresh());
        assert!(!RuntimeBinding::Unknown.needs_refresh());
    }

    #[test]
    fn a_stale_binding_is_advised_and_refreshed_only_from_the_current_runtime() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let runtime = newer_runtime(&plan);
        select_current(&plan, "v0.8.50-test");
        assert!(repair_advice(&plan.home, &plan.repository_uuid, plan.uid).is_none());
        assert_eq!(
            automatic_refresh_proof(
                &plan.home,
                &plan.repository_uuid,
                &plan.repository_uuid,
                &plan.repository,
                &plan.router,
            ),
            Err("recorded_runtime_current")
        );
        // The update selected a newer pair; the bound one stays installed.
        select_current(&plan, "v0.8.51-test");
        let advice = repair_advice(&plan.home, &plan.repository_uuid, plan.uid).unwrap();
        assert_eq!(advice.id, "storage.gc-service-runtime-stale");
        assert_eq!(advice.commands[0], REFRESH_COMMAND);
        assert!(advice.evidence[0].contains("0.8.50"));
        automatic_refresh_proof(
            &plan.home,
            &plan.repository_uuid,
            &plan.repository_uuid,
            &plan.repository,
            &runtime.router,
        )
        .unwrap();
        assert_eq!(
            automatic_refresh_proof(
                &plan.home,
                &plan.repository_uuid,
                &plan.repository_uuid,
                &plan.repository,
                &plan.router,
            ),
            Err("running_runtime_is_not_the_current_selection")
        );
        // `gc service status` names the refresh until it lands.
        let status = control_with(&plan.home, &plan.repository_uuid, "status", None, |_| {
            Ok(Observation::Unknown)
        })
        .unwrap();
        assert_eq!(status["runtime_binding"], "stale");
        assert_eq!(status["next_action"], REFRESH_COMMAND);
        // An unknown launchd answer changes nothing and the advice stays.
        let outcome = refresh_record_with(
            &plan.home,
            &plan.repository_uuid,
            plan.uid,
            Ok(&runtime),
            |_, _| Ok(LabelState::Unknown),
            |_| panic!("nothing is run without launchd's answer"),
        );
        assert_eq!(outcome.outcome, "label_state_unknown");
        assert_unchanged(&plan);
        assert!(repair_advice(&plan.home, &plan.repository_uuid, plan.uid).is_some());
        let outcome = refresh_record_with(
            &plan.home,
            &plan.repository_uuid,
            plan.uid,
            Ok(&runtime),
            |_, _| Ok(LabelState::Loaded),
            |_| Ok(Observation::Success),
        );
        assert_eq!(outcome.outcome, "refreshed_and_reloaded");
        assert!(repair_advice(&plan.home, &plan.repository_uuid, plan.uid).is_none());
        let fresh = refreshed_plan(&plan, &runtime);
        let status = control_with(&plan.home, &plan.repository_uuid, "status", None, |_| {
            Ok(Observation::Success)
        })
        .unwrap();
        assert_eq!(status["plan_digest"], fresh.digest);
        assert_eq!(status["runtime_binding"], "current");
    }

    #[test]
    fn refusing_ticks_are_advised_until_a_pass_finishes() {
        assert!(tick_refusing(Some(2), Some(1)));
        assert!(tick_refusing(Some(2), None));
        assert!(!tick_refusing(Some(1), Some(1)));
        assert!(!tick_refusing(Some(1), Some(2)));
        assert!(!tick_refusing(None, Some(2)));
        assert!(!tick_refusing(None, None));
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        write_private(
            &plan.directory.join("last-error.json"),
            &serde_json::to_vec(&json!({
                "schema": ERROR_SCHEMA,
                "repository_uuid": plan.repository_uuid,
                "generation": plan.generation,
                "observed_at_unix_ms": 2_000,
                "error": "managed GC tick preflight refused: fixture",
            }))
            .unwrap(),
        );
        let advice = repair_advice(&plan.home, &plan.repository_uuid, plan.uid).unwrap();
        assert_eq!(advice.id, "storage.gc-service-ticks-refusing");
        assert_eq!(advice.commands[0], REFRESH_COMMAND);
        assert!(advice.evidence.iter().any(|line| line.contains("fixture")));
        let status = control_with(&plan.home, &plan.repository_uuid, "status", None, |_| {
            Ok(Observation::Success)
        })
        .unwrap();
        assert_eq!(status["next_action"], REFRESH_COMMAND);
        // A pass that finished after the error clears it.
        write_private(
            &plan.receipt_path,
            &serde_json::to_vec(&json!({
                "schema": RECEIPT_SCHEMA,
                "repository_uuid": plan.repository_uuid,
                "generation": plan.generation,
                "started_at_unix_ms": 2_500,
                "finished_at_unix_ms": 3_000,
                "outcome": "sweep_complete",
                "directories_reclaimed": 0,
                "budget_ms": 1,
                "disk_pressured": false,
                "legacy_unscoped_state": "clear",
                "error": null,
            }))
            .unwrap(),
        );
        assert!(repair_advice(&plan.home, &plan.repository_uuid, plan.uid).is_none());
    }

    #[test]
    fn status_starts_one_background_refresh_per_interval_from_the_current_runtime() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        let runtime = newer_runtime(&plan);
        let never = || -> io::Result<()> { panic!("no refresh is started") };
        // No selector, or a current binding: nothing is started.
        assert_eq!(
            schedule_refresh_with(
                &plan.home,
                &plan.repository_uuid,
                plan.uid,
                &runtime.router,
                1,
                never
            ),
            None
        );
        select_current(&plan, "v0.8.50-test");
        assert_eq!(
            schedule_refresh_with(
                &plan.home,
                &plan.repository_uuid,
                plan.uid,
                &plan.router,
                1,
                never
            ),
            None
        );
        select_current(&plan, "v0.8.51-test");
        // Stale, but the caller is not the current runtime: nothing.
        assert_eq!(
            schedule_refresh_with(
                &plan.home,
                &plan.repository_uuid,
                plan.uid,
                &plan.router,
                1,
                never
            ),
            None
        );
        let now = 10_000_000;
        let mut started = 0;
        let scheduled = schedule_refresh_with(
            &plan.home,
            &plan.repository_uuid,
            plan.uid,
            &runtime.router,
            now,
            || {
                started += 1;
                Ok(())
            },
        );
        assert_eq!(scheduled, Some(ScheduledRefresh::Started));
        assert_eq!(started, 1);
        assert!(!ScheduledRefresh::Started.describe().is_empty());
        let receipt =
            read_bounded_json(&plan.directory.join(SCHEDULED_REFRESH_RECEIPT), plan.uid).unwrap();
        assert_eq!(receipt["observed_at_unix_ms"], now);
        // Within the interval: not again.
        assert_eq!(
            schedule_refresh_with(
                &plan.home,
                &plan.repository_uuid,
                plan.uid,
                &runtime.router,
                now + 60_000,
                never
            ),
            Some(ScheduledRefresh::RecentlyAttempted)
        );
        assert!(!ScheduledRefresh::RecentlyAttempted.describe().is_empty());
        // A refresh attempted by any path counts too.
        let mut attempt = RefreshOutcome::new(&plan.repository_uuid);
        attempt.outcome = "label_state_unknown";
        attempt.observed_at_unix_ms = now + 800_000;
        write_refresh_receipt(&plan.home, &plan.repository_uuid, &attempt).unwrap();
        assert_eq!(
            schedule_refresh_with(
                &plan.home,
                &plan.repository_uuid,
                plan.uid,
                &runtime.router,
                now + 1_000_000,
                never
            ),
            Some(ScheduledRefresh::RecentlyAttempted)
        );
        // After a full interval it starts again; a failed start is not one.
        assert_eq!(
            schedule_refresh_with(
                &plan.home,
                &plan.repository_uuid,
                plan.uid,
                &runtime.router,
                now + 2_000_000,
                || Err(io::Error::other("spawn refused"))
            ),
            None
        );
        assert_unchanged(&plan);
    }

    #[test]
    fn a_refused_tick_leaves_a_bounded_last_error() {
        let (_temp, plan) = fixture();
        install_fixture(&plan);
        write_tick_failure(
            &plan.home,
            &plan.repository_uuid,
            plan.uid,
            Some("not-a-digest"),
            &"x".repeat(32_768),
        )
        .unwrap();
        let path = plan.directory.join("last-error.json");
        assert!(fs::metadata(&path).unwrap().len() <= MAX_RECEIPT_BYTES as u64);
        let receipt = read_bounded_json(&path, plan.uid).unwrap();
        assert_eq!(receipt["schema"], ERROR_SCHEMA);
        assert!(receipt["generation"].is_null());
        assert_eq!(receipt["error"].as_str().unwrap().len(), 256);
        // The uuid comes from the environment: it must name a namespace.
        assert!(write_tick_failure(&plan.home, "../escape", plan.uid, None, "error").is_err());
        // An uninstalled repository gets no directory created for it.
        let other = "00000000-0000-4000-8000-000000000000";
        assert!(write_tick_failure(&plan.home, other, plan.uid, None, "error").is_err());
        assert!(!expected_directory(&plan.home, other).exists());
    }

    #[test]
    fn activation_freshness_compares_the_full_produced_plan_bytes() {
        let (_temp, plan) = fixture();
        let bytes = serde_json::to_vec(&plan).unwrap();
        assert!(matches_fresh_generation(&plan, &bytes).unwrap());
        let mut changed_runtime = plan.clone();
        let replacement = if changed_runtime.engine_sha256.starts_with('0') {
            "1"
        } else {
            "0"
        };
        changed_runtime
            .engine_sha256
            .replace_range(0..1, replacement);
        assert!(!matches_fresh_generation(&changed_runtime, &bytes).unwrap());
    }
}
