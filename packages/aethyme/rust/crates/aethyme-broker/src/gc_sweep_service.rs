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
    let _previous_pass = read_last_pass(plan)?;
    let finished_at_unix_ms = now_unix_ms()?;
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
        return Ok(json!({
            "schema": CONTROL_SCHEMA,
            "action": "status",
            "plan_digest": plan.digest,
            "generation": plan.generation,
            "scheduler_state": if observation == Observation::Success { "loaded" } else { "unknown" },
            "scheduler_observation": observation_name(&observation),
            "runtime_state": runtime_state,
            "control": control,
            "last_pass": last_pass,
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
    pub(crate) unit_present: bool,
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
        if self.runtime_present() == Some(false) {
            return Some("recorded_runtime_missing_and_label_not_loaded");
        }
        None
    }

    fn describe(&self, label: crate::scheduled_service::launchd::LabelState) -> Value {
        json!({
            "recorded_plan_digest": self.plan_digest,
            "recorded_runtime_version": self.runtime_version,
            "recorded_runtime_present": self.runtime_present(),
            "unit_file": if self.unit_present { "present" } else { "missing" },
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
    Ok(InstallationRecord {
        directory,
        unit_path,
        label,
        uid,
        plan_digest: field("digest"),
        runtime_version: field("runtime_version"),
        router: field("router").map(PathBuf::from),
        plan_bytes,
        unit_present,
    })
}

/// Move a dead record's files (its plan, receipts and, when present, its
/// unit) into `archive/<ms>/` inside the service directory, keeping the
/// newest [`RECORD_ARCHIVES_KEPT`] archives. Renames only; nothing of the
/// record is deleted here.
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
    let target = archives.join(now_unix_ms()?.to_string());
    fs::DirBuilder::new().mode(0o700).create(&target)?;
    for name in ["plan.json", "control.json", "last-pass.json"] {
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
    if record.unit_present {
        let metadata = fs::symlink_metadata(&record.unit_path)?;
        if !metadata.is_file() {
            return Err(invalid("GC service unit path is not a file"));
        }
        fs::rename(&record.unit_path, target.join("unit.plist"))?;
        if let Some(parent) = record.unit_path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
    }
    fs::File::open(&target)?.sync_all()?;
    fs::File::open(&record.directory)?.sync_all()?;
    let mut kept: Vec<(u64, PathBuf)> = fs::read_dir(&archives)?
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
    crate::phase_telemetry::add("gc_service_record_archived", Duration::ZERO);
    Ok(target)
}

/// Install `plan`, first archiving a different recorded installation when
/// it is provably dead; a live or undecidable one keeps the refusal.
fn install_replacing_dead_record(
    plan: &Plan,
    mut label_state: impl FnMut(u32, &str) -> io::Result<crate::scheduled_service::launchd::LabelState>,
) -> io::Result<(Vec<PathBuf>, Option<PathBuf>)> {
    let new_bytes = serde_json::to_vec(plan).map_err(io::Error::other)?;
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
    Ok((install_exact_plan_files(plan)?, archived))
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
    if record.unit_present || record.plan_bytes.is_none() {
        return Ok(None);
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
        "uninstall" => {
            if confirmation.is_none() || confirmation != record.plan_digest.as_deref() {
                return Err(invalid(
                    "GC service uninstall requires the exact recorded plan digest",
                ));
            }
            if record.dead_reason(state).is_none() {
                return Err(invalid(
                    "GC service record has no unit file but launchd did not confirm the label is unloaded; refusing to remove it",
                ));
            }
            let _claim = crate::pr_watch_service_install::claim_installer(&record.directory, uid)?;
            let archive = archive_record(&record)?;
            Ok(Some(json!({
                "schema": CONTROL_SCHEMA,
                "action": "uninstall",
                "plan_digest": record.plan_digest,
                "scheduler_state": "not_loaded",
                "installed_files_removed": true,
                "stale_record_archived_to": archive,
                "delivery_proven": false
            })))
        }
        _ => Ok(None),
    }
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
