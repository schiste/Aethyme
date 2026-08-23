//! Explicit installation provenance and update planning.
//!
//! The planner is pure once manifest bytes and detected installation state are
//! supplied. Network access and plan persistence live at the CLI boundary.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{ReleaseArtifact, ReleaseCompatibility, ReleaseManifest};

pub const UPDATE_PLAN_SCHEMA_VERSION: u32 = 1;
pub const INSTALL_RECEIPT_SCHEMA_VERSION: u32 = 1;
pub const INSTALL_RECEIPT_FILENAME: &str = "install-receipt.json";
const DEFAULT_RELEASE_BASE_URL: &str = "https://github.com/schiste/Aethyme";
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    Stable,
    Preview,
}

impl UpdateChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Preview => "preview",
        }
    }
}

impl std::str::FromStr for UpdateChannel {
    type Err = UpdateError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "stable" => Ok(Self::Stable),
            "preview" => Ok(Self::Preview),
            _ => Err(UpdateError::InvalidChannel(value.to_string())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationMethod {
    Homebrew,
    Installer,
    Cargo,
    ManualArchive,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallReceipt {
    pub schema_version: u32,
    pub method: String,
    pub install_dir: PathBuf,
    pub managed_root: PathBuf,
    pub router_path: PathBuf,
    pub engine_path: PathBuf,
    pub current_link: PathBuf,
    pub previous_link: PathBuf,
    pub versions_dir: PathBuf,
}

impl InstallReceipt {
    pub fn validate(&self) -> Result<(), UpdateError> {
        if self.schema_version != INSTALL_RECEIPT_SCHEMA_VERSION {
            return Err(UpdateError::InvalidReceipt(format!(
                "unsupported schema {}",
                self.schema_version
            )));
        }
        if self.method != "aethyme-installer" {
            return Err(UpdateError::InvalidReceipt("unknown install method".into()));
        }
        for path in [
            &self.install_dir,
            &self.managed_root,
            &self.router_path,
            &self.engine_path,
            &self.current_link,
            &self.previous_link,
            &self.versions_dir,
        ] {
            if !path.is_absolute() {
                return Err(UpdateError::InvalidReceipt(format!(
                    "{} is not absolute",
                    path.display()
                )));
            }
        }
        if self.current_link != self.managed_root.join("current")
            || self.previous_link != self.managed_root.join("previous")
            || self.versions_dir != self.managed_root.join("versions")
            || self.managed_root != self.install_dir.join(".aethyme-managed")
            || self.router_path != self.install_dir.join("aethyme")
            || self.engine_path != self.install_dir.join("aethyme-engine-cli")
        {
            return Err(UpdateError::InvalidReceipt(
                "managed paths do not share the declared installation root".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallationProvenance {
    pub method: InstallationMethod,
    pub router_path: PathBuf,
    pub engine_path: Option<PathBuf>,
    pub managed_root: Option<PathBuf>,
    pub receipt_path: Option<PathBuf>,
    pub manager_command: Option<String>,
    pub explanation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateAction {
    UpToDate,
    ExecuteInstallerUpdate,
    RunHomebrewUpgrade,
    ReinstallFromCargo,
    AdoptInstaller,
    RefuseDowngrade,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateArchive {
    pub archive: String,
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePlan {
    pub schema_version: u32,
    pub created_at_unix_ms: i64,
    pub channel: UpdateChannel,
    pub manifest_sha256: String,
    pub manifest_url: String,
    pub archive: UpdateArchive,
    pub current_version: String,
    pub target_version: String,
    pub source_sha: String,
    pub compatibility: ReleaseCompatibility,
    pub installation: InstallationProvenance,
    pub action: UpdateAction,
    pub recommended_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateExecutionReport {
    pub manifest_sha256: String,
    pub installed_version: String,
    pub installed_target: String,
    pub router_path: PathBuf,
    pub engine_path: PathBuf,
    pub active_bundle: PathBuf,
    pub rollback_bundle: Option<PathBuf>,
    pub quick_test_passed: bool,
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("invalid update channel {0:?}; expected stable or preview")]
    InvalidChannel(String),
    #[error("invalid installation receipt: {0}")]
    InvalidReceipt(String),
    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse {path}: {source}")]
    ParseReceipt {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("parse release manifest: {0}")]
    ParseManifest(serde_json::Error),
    #[error("invalid release manifest: {0}")]
    InvalidManifest(String),
    #[error("release manifest channel is {actual}, not requested {requested}")]
    ChannelMismatch { requested: String, actual: String },
    #[error("release manifest has no artifact for {0}")]
    UnsupportedTarget(String),
    #[error("invalid current version {0:?}")]
    InvalidCurrentVersion(String),
    #[error("invalid target version {0:?}")]
    InvalidTargetVersion(String),
    #[error("unsupported platform {os} {arch}")]
    UnsupportedPlatform { os: String, arch: String },
}

pub fn current_release_target() -> Result<&'static str, UpdateError> {
    release_target_for(std::env::consts::OS, std::env::consts::ARCH)
}

pub fn release_target_for(os: &str, arch: &str) -> Result<&'static str, UpdateError> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        _ => Err(UpdateError::UnsupportedPlatform {
            os: os.to_string(),
            arch: arch.to_string(),
        }),
    }
}

pub fn detect_installation(executable: &Path) -> InstallationProvenance {
    let executable = fs::canonicalize(executable).unwrap_or_else(|_| executable.to_path_buf());
    if let Some((receipt_path, receipt)) = find_receipt(&executable) {
        if receipt.validate().is_ok()
            && fs::canonicalize(&receipt.router_path).ok().as_ref() == Some(&executable)
        {
            return InstallationProvenance {
                method: InstallationMethod::Installer,
                router_path: receipt.router_path,
                engine_path: Some(receipt.engine_path),
                managed_root: Some(receipt.managed_root),
                receipt_path: Some(receipt_path),
                manager_command: None,
                explanation: "installer receipt owns the paired binary layout".into(),
            };
        }
    }

    let parent = executable.parent().unwrap_or(Path::new(""));
    let engine = parent.join("aethyme-engine-cli");
    if is_homebrew_cellar_path(&executable) {
        return InstallationProvenance {
            method: InstallationMethod::Homebrew,
            router_path: executable,
            engine_path: engine.is_file().then_some(engine),
            managed_root: None,
            receipt_path: None,
            manager_command: Some("brew upgrade aethyme".into()),
            explanation: "executable resolves inside Homebrew's Cellar".into(),
        };
    }
    if is_cargo_bin_path(&executable) {
        return InstallationProvenance {
            method: InstallationMethod::Cargo,
            router_path: executable,
            engine_path: engine.is_file().then_some(engine),
            managed_root: None,
            receipt_path: None,
            manager_command: Some(
                "cargo install --locked --path packages/aethyme/rust/crates/aethyme-cli && cargo install --locked --path packages/aethyme/rust/crates/aethyme-engine".into(),
            ),
            explanation: "executable resolves inside Cargo's bin directory".into(),
        };
    }
    if engine.is_file() {
        return InstallationProvenance {
            method: InstallationMethod::ManualArchive,
            router_path: executable,
            engine_path: Some(engine),
            managed_root: None,
            receipt_path: None,
            manager_command: None,
            explanation: "unmanaged sibling binary found beside the router".into(),
        };
    }
    InstallationProvenance {
        method: InstallationMethod::Unknown,
        router_path: executable,
        engine_path: None,
        managed_root: None,
        receipt_path: None,
        manager_command: None,
        explanation: "no Homebrew, installer, Cargo, or paired-archive provenance found".into(),
    }
}

pub fn build_update_plan(
    manifest_bytes: &[u8],
    channel: UpdateChannel,
    installation: InstallationProvenance,
    current_version: &str,
    target: &str,
    release_base_url: &str,
    manifest_url: &str,
    created_at_unix_ms: i64,
) -> Result<UpdatePlan, UpdateError> {
    let manifest: ReleaseManifest =
        serde_json::from_slice(manifest_bytes).map_err(UpdateError::ParseManifest)?;
    manifest.validate().map_err(UpdateError::InvalidManifest)?;
    if manifest.release_channel != channel.as_str() {
        return Err(UpdateError::ChannelMismatch {
            requested: channel.as_str().to_string(),
            actual: manifest.release_channel,
        });
    }
    let artifact = manifest
        .artifact_for_target(target)
        .ok_or_else(|| UpdateError::UnsupportedTarget(target.to_string()))?;
    let current = Version::parse(current_version)
        .map_err(|_| UpdateError::InvalidCurrentVersion(current_version.to_string()))?;
    let target_version = Version::parse(&manifest.version)
        .map_err(|_| UpdateError::InvalidTargetVersion(manifest.version.clone()))?;
    let (action, recommended_command) = if target_version < current {
        (UpdateAction::RefuseDowngrade, None)
    } else if target_version == current {
        (UpdateAction::UpToDate, None)
    } else {
        action_for_installation(installation.method, installation.manager_command.clone())
    };

    Ok(UpdatePlan {
        schema_version: UPDATE_PLAN_SCHEMA_VERSION,
        created_at_unix_ms,
        channel,
        manifest_sha256: sha256_bytes(manifest_bytes),
        manifest_url: manifest_url.to_string(),
        archive: archive_plan(artifact, release_base_url, &manifest.version),
        current_version: current_version.to_string(),
        target_version: manifest.version,
        source_sha: manifest.source_sha,
        compatibility: manifest.compatibility,
        installation,
        action,
        recommended_command,
    })
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn run_update_cli(args: &[String]) -> u8 {
    match run_update_cli_inner(args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Error: {error}");
            1
        }
    }
}

fn run_update_cli_inner(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("--help" | "-h") | None => {
            print_update_help();
            Ok(())
        }
        Some("check") => {
            let json = parse_json_only(&args[1..])?;
            let plan = resolve_update_plan(UpdateChannel::Stable)?;
            render_plan(&plan, json, false, None)
        }
        Some("plan") => {
            let (channel, json) = parse_plan_options(&args[1..])?;
            let plan = resolve_update_plan(channel)?;
            let saved_to = persist_update_plan(&plan)?;
            render_plan(&plan, json, true, saved_to.as_deref())
        }
        Some("execute") => {
            let (confirmation, json) = parse_execute_options(&args[1..])?;
            let report = execute_confirmed_update(&confirmation)?;
            render_execution(&report, json)
        }
        Some("bootstrap") => run_bootstrap(&args[1..]),
        Some(other) => Err(format!(
            "unsupported update subcommand {other:?}; use check, plan, or execute"
        )),
    }
}

fn parse_execute_options(args: &[String]) -> Result<(String, bool), String> {
    let mut confirmation = None;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            "--confirm" => {
                confirmation = Some(
                    args.get(index + 1)
                        .ok_or("update execute: --confirm requires a value")?
                        .clone(),
                );
                index += 2;
            }
            other => return Err(format!("update execute: unknown option {other}")),
        }
    }
    let confirmation = confirmation.ok_or("update execute requires --confirm <manifest-sha256>")?;
    validate_digest(&confirmation, "confirmation")?;
    Ok((confirmation, json))
}

fn run_bootstrap(args: &[String]) -> Result<(), String> {
    let mut payload = None;
    let mut install_dir = None;
    let mut manifest_path = None;
    let mut target = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("update bootstrap: {flag} requires a value"))?;
        match flag {
            "--payload" => payload = Some(PathBuf::from(value)),
            "--install-dir" => install_dir = Some(PathBuf::from(value)),
            "--manifest" => manifest_path = Some(PathBuf::from(value)),
            "--target" => target = Some(value.clone()),
            _ => return Err(format!("update bootstrap: unknown option {flag}")),
        }
        index += 2;
    }
    let report = bootstrap_install(
        &payload.ok_or("update bootstrap requires --payload")?,
        &install_dir.ok_or("update bootstrap requires --install-dir")?,
        &manifest_path.ok_or("update bootstrap requires --manifest")?,
        &target.ok_or("update bootstrap requires --target")?,
    )?;
    println!(
        "Installed Aethyme {} ({}) to {}",
        report.installed_version,
        report.installed_target,
        report
            .router_path
            .parent()
            .unwrap_or(Path::new("."))
            .display()
    );
    Ok(())
}

fn print_update_help() {
    eprintln!("aethyme update — explicit paired-binary updates (never runs in the background)");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  aethyme update check [--json]");
    eprintln!("  aethyme update plan [--channel stable|preview] [--json]");
    eprintln!("  aethyme update execute --confirm <manifest-sha256> [--json]");
    eprintln!();
    eprintln!("Homebrew installs are updated with `brew upgrade aethyme`.");
}

fn parse_json_only(args: &[String]) -> Result<bool, String> {
    match args {
        [] => Ok(false),
        [flag] if flag == "--json" => Ok(true),
        _ => Err("update check accepts only --json".into()),
    }
}

fn parse_plan_options(args: &[String]) -> Result<(UpdateChannel, bool), String> {
    let mut channel = UpdateChannel::Stable;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" => {
                json = true;
                index += 1;
            }
            "--channel" => {
                let value = args
                    .get(index + 1)
                    .ok_or("update plan: --channel requires a value")?;
                channel = value
                    .parse()
                    .map_err(|error: UpdateError| error.to_string())?;
                index += 2;
            }
            other => return Err(format!("update plan: unknown option {other}")),
        }
    }
    Ok((channel, json))
}

fn resolve_update_plan(channel: UpdateChannel) -> Result<UpdatePlan, String> {
    let base_url = std::env::var("AETHYME_RELEASE_BASE_URL")
        .unwrap_or_else(|_| DEFAULT_RELEASE_BASE_URL.to_string());
    let manifest_url = update_manifest_url(&base_url, channel);
    let manifest_bytes = fetch_bounded(&manifest_url, MAX_MANIFEST_BYTES)?;
    let executable = std::env::current_exe().map_err(|error| format!("locate aethyme: {error}"))?;
    let installation = detect_installation(&executable);
    let target = current_release_target().map_err(|error| error.to_string())?;
    build_update_plan(
        &manifest_bytes,
        channel,
        installation,
        env!("CARGO_PKG_VERSION"),
        target,
        &base_url,
        &manifest_url,
        now_unix_ms(),
    )
    .map_err(|error| error.to_string())
}

fn update_manifest_url(base_url: &str, channel: UpdateChannel) -> String {
    let base = base_url.trim_end_matches('/');
    match channel {
        UpdateChannel::Stable => {
            format!("{base}/releases/latest/download/release-manifest.json")
        }
        UpdateChannel::Preview => {
            format!("{base}/releases/download/preview/release-manifest.json")
        }
    }
}

fn fetch_bounded(url: &str, maximum_bytes: u64) -> Result<Vec<u8>, String> {
    let temp = tempfile::tempdir().map_err(|error| format!("create update temp dir: {error}"))?;
    let destination = temp.path().join("download");
    download_to(url, &destination)?;
    let metadata = fs::metadata(&destination)
        .map_err(|error| format!("stat downloaded update metadata: {error}"))?;
    if metadata.len() > maximum_bytes {
        return Err(format!(
            "downloaded update metadata is {} bytes; limit is {maximum_bytes}",
            metadata.len()
        ));
    }
    fs::read(&destination).map_err(|error| format!("read downloaded update metadata: {error}"))
}

fn download_to(url: &str, destination: &Path) -> Result<(), String> {
    if let Some(path) = url.strip_prefix("file://") {
        fs::copy(path, destination)
            .map(|_| ())
            .map_err(|error| format!("copy {url}: {error}"))
    } else {
        if !url.starts_with("https://") {
            return Err(format!("refusing non-HTTPS update URL {url}"));
        }
        let output = Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--proto",
                "=https",
                "--output",
            ])
            .arg(destination)
            .arg(url)
            .output()
            .map_err(|error| format!("run curl: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "download {url}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
}

fn persist_update_plan(plan: &UpdatePlan) -> Result<Option<PathBuf>, String> {
    if plan.action != UpdateAction::ExecuteInstallerUpdate {
        return Ok(None);
    }
    let root = plan
        .installation
        .managed_root
        .as_ref()
        .ok_or("installer update plan has no managed root")?;
    let directory = root.join("update-plans");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    let destination = directory.join(format!("{}.json", plan.manifest_sha256));
    let mut encoded =
        serde_json::to_vec_pretty(plan).map_err(|error| format!("encode update plan: {error}"))?;
    encoded.push(b'\n');
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)
        .map_err(|error| format!("create update plan temp file: {error}"))?;
    temporary
        .write_all(&encoded)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| format!("write update plan: {error}"))?;
    temporary
        .persist(&destination)
        .map_err(|error| format!("publish {}: {}", destination.display(), error.error))?;
    Ok(Some(destination))
}

fn render_plan(
    plan: &UpdatePlan,
    json: bool,
    detailed: bool,
    saved_to: Option<&Path>,
) -> Result<(), String> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(plan)
                .map_err(|error| format!("encode update plan: {error}"))?
        );
        return Ok(());
    }
    if detailed {
        println!("Aethyme update plan");
        println!("  Current:      {}", plan.current_version);
        println!(
            "  Target:       {} ({})",
            plan.target_version,
            plan.channel.as_str()
        );
        println!("  Installation: {:?}", plan.installation.method);
        println!("  Manifest:     {}", plan.manifest_sha256);
        println!("  Archive:      {}", plan.archive.archive);
        println!("  Action:       {:?}", plan.action);
        if let Some(path) = saved_to {
            println!("  Reviewed plan: {}", path.display());
            println!(
                "Next: aethyme update execute --confirm {}",
                plan.manifest_sha256
            );
        } else if let Some(command) = &plan.recommended_command {
            println!("Next: {command}");
        }
    } else {
        println!(
            "Aethyme {} ({:?}); {} {} -> {:?}",
            plan.current_version,
            plan.installation.method,
            plan.channel.as_str(),
            plan.target_version,
            plan.action
        );
        if let Some(command) = &plan.recommended_command {
            println!("Next: {command}");
        }
    }
    Ok(())
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub fn bootstrap_install(
    payload: &Path,
    install_dir: &Path,
    manifest_path: &Path,
    target: &str,
) -> Result<UpdateExecutionReport, String> {
    let manifest_bytes = fs::read(manifest_path)
        .map_err(|error| format!("read {}: {error}", manifest_path.display()))?;
    let manifest = parse_valid_manifest(&manifest_bytes)?;
    let artifact = manifest
        .artifact_for_target(target)
        .ok_or_else(|| format!("release manifest has no artifact for {target}"))?;
    let installed_target = artifact.target.clone();
    verify_payload(payload, &manifest.version)?;

    fs::create_dir_all(install_dir)
        .map_err(|error| format!("create {}: {error}", install_dir.display()))?;
    let install_dir = fs::canonicalize(install_dir)
        .map_err(|error| format!("resolve {}: {error}", install_dir.display()))?;
    let managed_root = install_dir.join(".aethyme-managed");
    let receipt = InstallReceipt {
        schema_version: INSTALL_RECEIPT_SCHEMA_VERSION,
        method: "aethyme-installer".into(),
        install_dir: install_dir.clone(),
        managed_root: managed_root.clone(),
        router_path: install_dir.join("aethyme"),
        engine_path: install_dir.join("aethyme-engine-cli"),
        current_link: managed_root.join("current"),
        previous_link: managed_root.join("previous"),
        versions_dir: managed_root.join("versions"),
    };
    receipt.validate().map_err(|error| error.to_string())?;
    let manifest_sha256 = sha256_bytes(&manifest_bytes);
    let (active_bundle, rollback_bundle) =
        activate_payload(payload, &receipt, &manifest.version, &manifest_sha256)?;
    write_receipt(&receipt)?;
    Ok(UpdateExecutionReport {
        manifest_sha256,
        installed_version: manifest.version,
        installed_target,
        router_path: receipt.router_path,
        engine_path: receipt.engine_path,
        active_bundle,
        rollback_bundle,
        quick_test_passed: true,
    })
}

pub fn execute_confirmed_update(confirmation: &str) -> Result<UpdateExecutionReport, String> {
    validate_digest(confirmation, "confirmation")?;
    let executable = std::env::current_exe().map_err(|error| format!("locate aethyme: {error}"))?;
    let installation = detect_installation(&executable);
    if installation.method != InstallationMethod::Installer {
        return Err(format!(
            "self-update is available only for installer-managed binaries; detected {:?}",
            installation.method
        ));
    }
    let receipt_path = installation
        .receipt_path
        .as_ref()
        .ok_or("installer provenance has no receipt")?;
    let receipt = read_receipt(receipt_path)?;
    let plan_path = receipt
        .managed_root
        .join("update-plans")
        .join(format!("{confirmation}.json"));
    let plan_bytes = read_bounded_file(&plan_path, MAX_MANIFEST_BYTES)?;
    let plan: UpdatePlan = serde_json::from_slice(&plan_bytes)
        .map_err(|error| format!("parse {}: {error}", plan_path.display()))?;
    validate_saved_plan(&plan, confirmation, &installation)?;

    let temp = tempfile::tempdir().map_err(|error| format!("create update temp dir: {error}"))?;
    let manifest_path = temp.path().join("release-manifest.json");
    download_to(&plan.manifest_url, &manifest_path)?;
    let manifest_bytes = read_bounded_file(&manifest_path, MAX_MANIFEST_BYTES)?;
    if sha256_bytes(&manifest_bytes) != confirmation {
        return Err(
            "release manifest changed after review; run `aethyme update plan` again".into(),
        );
    }
    let manifest = parse_valid_manifest(&manifest_bytes)?;
    validate_manifest_against_plan(&manifest, &plan)?;
    inspect_current_broker_schema(&manifest.compatibility)?;

    let archive_path = temp.path().join(&plan.archive.archive);
    download_to(&plan.archive.url, &archive_path)?;
    let archive_size = fs::metadata(&archive_path)
        .map_err(|error| format!("stat {}: {error}", archive_path.display()))?
        .len();
    if archive_size > MAX_ARCHIVE_BYTES {
        return Err(format!(
            "release archive is {archive_size} bytes; safety limit is {MAX_ARCHIVE_BYTES}"
        ));
    }
    if archive_size != plan.archive.size_bytes {
        return Err(format!(
            "release archive size mismatch: planned {}, downloaded {archive_size}",
            plan.archive.size_bytes
        ));
    }
    let archive_digest = sha256_file(&archive_path)?;
    if archive_digest != plan.archive.sha256 {
        return Err(format!(
            "release archive SHA-256 mismatch: planned {}, downloaded {archive_digest}",
            plan.archive.sha256
        ));
    }
    verify_archive_members(&archive_path)?;
    let payload = temp.path().join("payload");
    fs::create_dir(&payload).map_err(|error| format!("create payload directory: {error}"))?;
    run_tar(&["-xzf"], &archive_path, Some(&payload))?;
    verify_payload(&payload, &plan.target_version)?;

    let (active_bundle, rollback_bundle) =
        activate_payload(&payload, &receipt, &plan.target_version, confirmation)?;
    fs::remove_file(&plan_path)
        .map_err(|error| format!("remove consumed plan {}: {error}", plan_path.display()))?;
    Ok(UpdateExecutionReport {
        manifest_sha256: confirmation.to_string(),
        installed_version: plan.target_version,
        installed_target: plan.archive.target,
        router_path: receipt.router_path,
        engine_path: receipt.engine_path,
        active_bundle,
        rollback_bundle,
        quick_test_passed: true,
    })
}

fn render_execution(report: &UpdateExecutionReport, json: bool) -> Result<(), String> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(report)
                .map_err(|error| format!("encode update result: {error}"))?
        );
    } else {
        println!(
            "Updated Aethyme to {} ({})",
            report.installed_version, report.installed_target
        );
        println!("  Router: {}", report.router_path.display());
        println!("  Engine: {}", report.engine_path.display());
        if let Some(path) = &report.rollback_bundle {
            println!("  Rollback bundle: {}", path.display());
        }
        println!("  Quick test: passed");
    }
    Ok(())
}

fn validate_saved_plan(
    plan: &UpdatePlan,
    confirmation: &str,
    installation: &InstallationProvenance,
) -> Result<(), String> {
    if plan.schema_version != UPDATE_PLAN_SCHEMA_VERSION {
        return Err(format!(
            "unsupported saved update plan schema {}",
            plan.schema_version
        ));
    }
    if plan.manifest_sha256 != confirmation {
        return Err("saved update plan does not match the confirmed manifest digest".into());
    }
    if plan.action != UpdateAction::ExecuteInstallerUpdate {
        return Err(format!(
            "saved update action {:?} is not executable",
            plan.action
        ));
    }
    if plan.current_version != env!("CARGO_PKG_VERSION") {
        return Err(format!(
            "running version moved since planning: planned {}, running {}",
            plan.current_version,
            env!("CARGO_PKG_VERSION")
        ));
    }
    if &plan.installation != installation {
        return Err("installation provenance changed since planning".into());
    }
    Ok(())
}

fn validate_manifest_against_plan(
    manifest: &ReleaseManifest,
    plan: &UpdatePlan,
) -> Result<(), String> {
    if manifest.release_channel != plan.channel.as_str()
        || manifest.version != plan.target_version
        || manifest.source_sha != plan.source_sha
        || manifest.compatibility != plan.compatibility
    {
        return Err("confirmed manifest no longer matches the reviewed update plan".into());
    }
    let artifact = manifest
        .artifact_for_target(&plan.archive.target)
        .ok_or("confirmed manifest no longer supports the planned target")?;
    if artifact.archive != plan.archive.archive
        || artifact.sha256 != plan.archive.sha256
        || artifact.size_bytes != plan.archive.size_bytes
    {
        return Err(
            "confirmed manifest artifact no longer matches the reviewed update plan".into(),
        );
    }
    Ok(())
}

fn parse_valid_manifest(bytes: &[u8]) -> Result<ReleaseManifest, String> {
    let manifest: ReleaseManifest = serde_json::from_slice(bytes)
        .map_err(|error| format!("parse release manifest: {error}"))?;
    manifest
        .validate()
        .map_err(|error| format!("invalid release manifest: {error}"))?;
    Ok(manifest)
}

fn read_receipt(path: &Path) -> Result<InstallReceipt, String> {
    let bytes = read_bounded_file(path, MAX_MANIFEST_BYTES)?;
    let receipt: InstallReceipt = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    receipt.validate().map_err(|error| error.to_string())?;
    Ok(receipt)
}

fn write_receipt(receipt: &InstallReceipt) -> Result<(), String> {
    fs::create_dir_all(&receipt.managed_root)
        .map_err(|error| format!("create {}: {error}", receipt.managed_root.display()))?;
    let destination = receipt.managed_root.join(INSTALL_RECEIPT_FILENAME);
    let mut bytes = serde_json::to_vec_pretty(receipt)
        .map_err(|error| format!("encode install receipt: {error}"))?;
    bytes.push(b'\n');
    let mut temporary = tempfile::NamedTempFile::new_in(&receipt.managed_root)
        .map_err(|error| format!("create install receipt temp file: {error}"))?;
    temporary
        .write_all(&bytes)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| format!("write install receipt: {error}"))?;
    temporary
        .persist(&destination)
        .map_err(|error| format!("publish {}: {}", destination.display(), error.error))?;
    sync_directory(&receipt.managed_root)
}

fn activate_payload(
    payload: &Path,
    receipt: &InstallReceipt,
    version: &str,
    manifest_sha256: &str,
) -> Result<(PathBuf, Option<PathBuf>), String> {
    receipt.validate().map_err(|error| error.to_string())?;
    validate_digest(manifest_sha256, "manifest")?;
    verify_payload(payload, version)?;
    fs::create_dir_all(&receipt.versions_dir)
        .map_err(|error| format!("create {}: {error}", receipt.versions_dir.display()))?;

    let bundle_name = format!("v{version}-{}", &manifest_sha256[..12]);
    let final_bundle = receipt.versions_dir.join(&bundle_name);
    if final_bundle.exists() {
        verify_existing_bundle(payload, &final_bundle)?;
    } else {
        let staging = receipt
            .versions_dir
            .join(format!(".staging-{bundle_name}-{}", std::process::id()));
        if staging.exists() {
            fs::remove_dir_all(&staging)
                .map_err(|error| format!("clear stale staging {}: {error}", staging.display()))?;
        }
        fs::create_dir(&staging)
            .map_err(|error| format!("create staging {}: {error}", staging.display()))?;
        for binary in ["aethyme", "aethyme-engine-cli"] {
            let destination = staging.join(binary);
            fs::copy(payload.join(binary), &destination)
                .map_err(|error| format!("stage {binary}: {error}"))?;
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))
                .map_err(|error| format!("make staged {binary} executable: {error}"))?;
            fs::File::open(&destination)
                .and_then(|file| file.sync_all())
                .map_err(|error| format!("sync staged {binary}: {error}"))?;
        }
        fs::write(
            staging.join("manifest.sha256"),
            format!("{manifest_sha256}\n"),
        )
        .map_err(|error| format!("write staged manifest digest: {error}"))?;
        verify_payload(&staging, version)?;
        fs::rename(&staging, &final_bundle)
            .map_err(|error| format!("publish staged bundle: {error}"))?;
        sync_directory(&receipt.versions_dir)?;
    }

    let relative_target = PathBuf::from("versions").join(&bundle_name);
    let old_target = fs::read_link(&receipt.current_link).ok();
    if let Some(old) = &old_target {
        if old != &relative_target {
            atomic_symlink(old, &receipt.previous_link)?;
        }
    }
    atomic_symlink(&relative_target, &receipt.current_link)?;
    ensure_public_links(receipt)?;
    if let Err(error) = verify_public_pair(receipt, version) {
        if let Some(old) = &old_target {
            atomic_symlink(old, &receipt.current_link)?;
        }
        return Err(format!(
            "activation verification failed and was rolled back: {error}"
        ));
    }
    cleanup_old_bundles(receipt)?;
    let rollback_bundle = fs::read_link(&receipt.previous_link)
        .ok()
        .map(|path| receipt.managed_root.join(path));
    Ok((final_bundle, rollback_bundle))
}

fn ensure_public_links(receipt: &InstallReceipt) -> Result<(), String> {
    atomic_symlink(
        Path::new(".aethyme-managed/current/aethyme"),
        &receipt.router_path,
    )?;
    atomic_symlink(
        Path::new(".aethyme-managed/current/aethyme-engine-cli"),
        &receipt.engine_path,
    )
}

fn atomic_symlink(target: &Path, destination: &Path) -> Result<(), String> {
    let parent = destination
        .parent()
        .ok_or_else(|| format!("{} has no parent", destination.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("create {}: {error}", parent.display()))?;
    let name = destination
        .file_name()
        .ok_or_else(|| format!("{} has no filename", destination.display()))?
        .to_string_lossy();
    let temporary = parent.join(format!(
        ".{name}.new-{}-{}",
        std::process::id(),
        now_unix_ms()
    ));
    std::os::unix::fs::symlink(target, &temporary)
        .map_err(|error| format!("create symlink {}: {error}", temporary.display()))?;
    if let Err(error) = fs::rename(&temporary, destination) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "activate symlink {}: {error}",
            destination.display()
        ));
    }
    sync_directory(parent)
}

fn verify_existing_bundle(payload: &Path, bundle: &Path) -> Result<(), String> {
    for binary in ["aethyme", "aethyme-engine-cli"] {
        if sha256_file(&payload.join(binary))? != sha256_file(&bundle.join(binary))? {
            return Err(format!(
                "existing bundle {} differs from reviewed payload",
                bundle.display()
            ));
        }
    }
    Ok(())
}

fn cleanup_old_bundles(receipt: &InstallReceipt) -> Result<(), String> {
    let mut keep = Vec::new();
    for link in [&receipt.current_link, &receipt.previous_link] {
        if let Ok(target) = fs::read_link(link) {
            if let Some(name) = target.file_name() {
                keep.push(name.to_os_string());
            }
        }
    }
    for entry in fs::read_dir(&receipt.versions_dir)
        .map_err(|error| format!("read {}: {error}", receipt.versions_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("read version bundle: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?;
        if file_type.is_dir() && !file_type.is_symlink() && !keep.contains(&entry.file_name()) {
            fs::remove_dir_all(entry.path()).map_err(|error| {
                format!("remove old bundle {}: {error}", entry.path().display())
            })?;
        }
    }
    sync_directory(&receipt.versions_dir)
}

fn verify_payload(payload: &Path, version: &str) -> Result<(), String> {
    verify_binary_version(&payload.join("aethyme"), "aethyme", version)?;
    verify_binary_version(
        &payload.join("aethyme-engine-cli"),
        "aethyme-engine-cli",
        version,
    )?;
    let output = Command::new(payload.join("aethyme"))
        .args(["broker", "quick-test"])
        .output()
        .map_err(|error| format!("run staged aethyme broker quick-test: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "staged aethyme broker quick-test failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}

fn verify_public_pair(receipt: &InstallReceipt, version: &str) -> Result<(), String> {
    verify_binary_version(&receipt.router_path, "aethyme", version)?;
    verify_binary_version(&receipt.engine_path, "aethyme-engine-cli", version)
}

fn verify_binary_version(path: &Path, name: &str, version: &str) -> Result<(), String> {
    let output = Command::new(path)
        .arg("--version")
        .output()
        .map_err(|error| format!("run {} --version: {error}", path.display()))?;
    if !output.status.success() {
        return Err(format!("{name} --version failed"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.split_whitespace().nth(1) != Some(version) {
        return Err(format!(
            "{name} version does not match manifest {version}: {}",
            stdout.trim()
        ));
    }
    Ok(())
}

fn verify_archive_members(archive: &Path) -> Result<(), String> {
    let output = run_tar(&["-tzf"], archive, None)?;
    let members = output
        .lines()
        .map(|line| line.strip_prefix("./").unwrap_or(line))
        .collect::<Vec<_>>();
    if members != ["aethyme", "aethyme-engine-cli"] {
        return Err(format!(
            "release archive contains unexpected paths: {members:?}"
        ));
    }
    Ok(())
}

fn run_tar(args: &[&str], archive: &Path, destination: Option<&Path>) -> Result<String, String> {
    let mut command = Command::new("tar");
    command.args(args).arg(archive);
    if let Some(destination) = destination {
        command.arg("-C").arg(destination);
    }
    let output = command
        .output()
        .map_err(|error| format!("run tar: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "tar failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn inspect_current_broker_schema(compatibility: &ReleaseCompatibility) -> Result<(), String> {
    let cwd =
        std::env::current_dir().map_err(|error| format!("resolve current directory: {error}"))?;
    let database = cwd.join(".aethyme/broker.db");
    if !database.is_file() {
        return Ok(());
    }
    inspect_broker_schema_at(&database, compatibility)
}

fn inspect_broker_schema_at(
    database: &Path,
    compatibility: &ReleaseCompatibility,
) -> Result<(), String> {
    let connection = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("inspect broker schema {}: {error}", database.display()))?;
    let schema: i64 = connection
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("read broker schema {}: {error}", database.display()))?;
    let supported = &compatibility.broker_storage;
    if schema < supported.minimum_readable_schema || schema > supported.current_schema {
        return Err(format!(
            "broker schema {schema} is incompatible with target range {}..={}; run the update outside this repository or choose a compatible release",
            supported.minimum_readable_schema, supported.current_schema
        ));
    }
    Ok(())
}

fn read_bounded_file(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>, String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("stat {}: {error}", path.display()))?;
    if metadata.len() > maximum_bytes {
        return Err(format!(
            "{} is {} bytes; limit is {maximum_bytes}",
            path.display(),
            metadata.len()
        ));
    }
    fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_digest(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("{label} must be a full lowercase SHA-256"));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), String> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync directory {}: {error}", path.display()))
}

fn action_for_installation(
    method: InstallationMethod,
    manager_command: Option<String>,
) -> (UpdateAction, Option<String>) {
    match method {
        InstallationMethod::Homebrew => (
            UpdateAction::RunHomebrewUpgrade,
            Some(manager_command.unwrap_or_else(|| "brew upgrade aethyme".into())),
        ),
        InstallationMethod::Installer => (UpdateAction::ExecuteInstallerUpdate, None),
        InstallationMethod::Cargo => (UpdateAction::ReinstallFromCargo, manager_command),
        InstallationMethod::ManualArchive | InstallationMethod::Unknown => (
            UpdateAction::AdoptInstaller,
            Some(
                "curl -fsSL https://github.com/schiste/Aethyme/releases/latest/download/install.sh | sh"
                    .into(),
            ),
        ),
    }
}

fn archive_plan(
    artifact: &ReleaseArtifact,
    release_base_url: &str,
    version: &str,
) -> UpdateArchive {
    UpdateArchive {
        archive: artifact.archive.clone(),
        url: format!(
            "{}/releases/download/v{version}/{}",
            release_base_url.trim_end_matches('/'),
            artifact.archive
        ),
        sha256: artifact.sha256.clone(),
        size_bytes: artifact.size_bytes,
        target: artifact.target.clone(),
    }
}

fn find_receipt(executable: &Path) -> Option<(PathBuf, InstallReceipt)> {
    for ancestor in executable.ancestors().take(6) {
        let path = ancestor.join(INSTALL_RECEIPT_FILENAME);
        if !path.is_file() {
            continue;
        }
        let bytes = fs::read(&path).ok()?;
        let receipt = serde_json::from_slice(&bytes).ok()?;
        return Some((path, receipt));
    }
    None
}

fn is_homebrew_cellar_path(path: &Path) -> bool {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>();
    components
        .windows(2)
        .any(|pair| pair[0].eq_ignore_ascii_case("cellar") && pair[1] == "aethyme")
}

fn is_cargo_bin_path(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME") {
        if parent == PathBuf::from(cargo_home).join("bin") {
            return true;
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .is_some_and(|home| parent == home.join(".cargo/bin"))
}

#[cfg(test)]
mod tests {
    use crate::{RELEASE_TARGETS, REQUIRED_RELEASE_BINARIES, ReleaseInstaller};

    use super::*;

    fn manifest(channel: &str, version: &str) -> Vec<u8> {
        let manifest = ReleaseManifest::new(
            version,
            "a".repeat(40),
            channel,
            RELEASE_TARGETS
                .iter()
                .map(|target| ReleaseArtifact {
                    archive: format!("aethyme-v{version}-{target}.tar.gz"),
                    binaries: REQUIRED_RELEASE_BINARIES
                        .iter()
                        .map(|binary| (*binary).to_string())
                        .collect(),
                    sha256: "b".repeat(64),
                    size_bytes: 123,
                    target: (*target).to_string(),
                })
                .collect(),
            ReleaseInstaller {
                filename: "install.sh".into(),
                sha256: "c".repeat(64),
                size_bytes: 10,
            },
        );
        serde_json::to_vec_pretty(&manifest).unwrap()
    }

    fn provenance(method: InstallationMethod) -> InstallationProvenance {
        InstallationProvenance {
            method,
            router_path: "/opt/aethyme".into(),
            engine_path: Some("/opt/aethyme-engine-cli".into()),
            managed_root: None,
            receipt_path: None,
            manager_command: (method == InstallationMethod::Homebrew)
                .then(|| "brew upgrade aethyme".into()),
            explanation: "fixture".into(),
        }
    }

    fn fake_payload(root: &Path, version: &str, quick_test_passes: bool) -> PathBuf {
        let payload = root.join(format!("payload-{version}"));
        fs::create_dir(&payload).unwrap();
        let quick_exit = if quick_test_passes { 0 } else { 1 };
        fs::write(
            payload.join("aethyme"),
            format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'aethyme {version}'; exit 0; fi\nif [ \"$1\" = broker ] && [ \"$2\" = quick-test ]; then exit {quick_exit}; fi\nexit 2\n"
            ),
        )
        .unwrap();
        fs::write(
            payload.join("aethyme-engine-cli"),
            format!("#!/bin/sh\necho 'aethyme-engine-cli {version}'\n"),
        )
        .unwrap();
        for binary in ["aethyme", "aethyme-engine-cli"] {
            fs::set_permissions(payload.join(binary), fs::Permissions::from_mode(0o755)).unwrap();
        }
        payload
    }

    fn manifest_file(root: &Path, version: &str) -> PathBuf {
        let path = root.join(format!("manifest-{version}.json"));
        fs::write(&path, manifest("stable", version)).unwrap();
        path
    }

    #[test]
    fn maps_supported_platforms() {
        assert_eq!(
            release_target_for("macos", "aarch64").unwrap(),
            RELEASE_TARGETS[0]
        );
        assert_eq!(
            release_target_for("macos", "x86_64").unwrap(),
            RELEASE_TARGETS[1]
        );
        assert_eq!(
            release_target_for("linux", "x86_64").unwrap(),
            RELEASE_TARGETS[2]
        );
        assert!(release_target_for("linux", "aarch64").is_err());
    }

    #[test]
    fn plan_routes_updates_to_the_installation_authority() {
        let cases = [
            (
                InstallationMethod::Homebrew,
                UpdateAction::RunHomebrewUpgrade,
            ),
            (
                InstallationMethod::Installer,
                UpdateAction::ExecuteInstallerUpdate,
            ),
            (InstallationMethod::Cargo, UpdateAction::ReinstallFromCargo),
            (
                InstallationMethod::ManualArchive,
                UpdateAction::AdoptInstaller,
            ),
            (InstallationMethod::Unknown, UpdateAction::AdoptInstaller),
        ];
        for (method, expected) in cases {
            let plan = build_update_plan(
                &manifest("stable", "0.3.0"),
                UpdateChannel::Stable,
                provenance(method),
                "0.2.0",
                RELEASE_TARGETS[0],
                "https://github.com/schiste/Aethyme",
                "https://example.test/manifest.json",
                42,
            )
            .unwrap();
            assert_eq!(plan.action, expected);
            assert_eq!(plan.manifest_sha256.len(), 64);
            assert!(plan.archive.url.contains("/releases/download/v0.3.0/"));
        }
    }

    #[test]
    fn plan_reports_current_and_refuses_downgrades() {
        let current = build_update_plan(
            &manifest("stable", "0.2.0"),
            UpdateChannel::Stable,
            provenance(InstallationMethod::Installer),
            "0.2.0",
            RELEASE_TARGETS[0],
            "https://example.test",
            "https://example.test/manifest.json",
            1,
        )
        .unwrap();
        assert_eq!(current.action, UpdateAction::UpToDate);

        let downgrade = build_update_plan(
            &manifest("stable", "0.1.0"),
            UpdateChannel::Stable,
            provenance(InstallationMethod::Installer),
            "0.2.0",
            RELEASE_TARGETS[0],
            "https://example.test",
            "https://example.test/manifest.json",
            1,
        )
        .unwrap();
        assert_eq!(downgrade.action, UpdateAction::RefuseDowngrade);
    }

    #[test]
    fn plan_rejects_channel_and_target_mismatch() {
        let bytes = manifest("preview", "0.3.0");
        assert!(matches!(
            build_update_plan(
                &bytes,
                UpdateChannel::Stable,
                provenance(InstallationMethod::Installer),
                "0.2.0",
                RELEASE_TARGETS[0],
                "https://example.test",
                "https://example.test/manifest.json",
                1,
            ),
            Err(UpdateError::ChannelMismatch { .. })
        ));
        assert!(matches!(
            build_update_plan(
                &manifest("stable", "0.3.0"),
                UpdateChannel::Stable,
                provenance(InstallationMethod::Installer),
                "0.2.0",
                "unsupported-target",
                "https://example.test",
                "https://example.test/manifest.json",
                1,
            ),
            Err(UpdateError::UnsupportedTarget(_))
        ));
    }

    #[test]
    fn detects_installer_homebrew_and_manual_layouts() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("bin");
        let root = install_dir.join(".aethyme-managed");
        let version_dir = root.join("versions/v0.2.0-test");
        fs::create_dir_all(&version_dir).unwrap();
        let router = version_dir.join("aethyme");
        let engine = version_dir.join("aethyme-engine-cli");
        fs::write(&router, b"router").unwrap();
        fs::write(&engine, b"engine").unwrap();
        std::os::unix::fs::symlink("versions/v0.2.0-test", root.join("current")).unwrap();
        std::os::unix::fs::symlink(
            ".aethyme-managed/current/aethyme",
            install_dir.join("aethyme"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            ".aethyme-managed/current/aethyme-engine-cli",
            install_dir.join("aethyme-engine-cli"),
        )
        .unwrap();
        let receipt = InstallReceipt {
            schema_version: INSTALL_RECEIPT_SCHEMA_VERSION,
            method: "aethyme-installer".into(),
            install_dir: install_dir.clone(),
            managed_root: root.clone(),
            router_path: install_dir.join("aethyme"),
            engine_path: install_dir.join("aethyme-engine-cli"),
            current_link: root.join("current"),
            previous_link: root.join("previous"),
            versions_dir: root.join("versions"),
        };
        fs::write(
            root.join(INSTALL_RECEIPT_FILENAME),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        assert_eq!(
            detect_installation(&router).method,
            InstallationMethod::Installer
        );

        let cellar = temp.path().join("Cellar/aethyme/0.2.0/bin");
        fs::create_dir_all(&cellar).unwrap();
        let router = cellar.join("aethyme");
        fs::write(&router, b"router").unwrap();
        fs::write(cellar.join("aethyme-engine-cli"), b"engine").unwrap();
        assert_eq!(
            detect_installation(&router).method,
            InstallationMethod::Homebrew
        );

        let manual = temp.path().join("manual");
        fs::create_dir(&manual).unwrap();
        let router = manual.join("aethyme");
        fs::write(&router, b"router").unwrap();
        fs::write(manual.join("aethyme-engine-cli"), b"engine").unwrap();
        assert_eq!(
            detect_installation(&router).method,
            InstallationMethod::ManualArchive
        );
    }

    #[test]
    fn bootstrap_switches_the_pair_once_and_retains_one_rollback_bundle() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("bin");

        let first = bootstrap_install(
            &fake_payload(temp.path(), "0.2.0", true),
            &install_dir,
            &manifest_file(temp.path(), "0.2.0"),
            RELEASE_TARGETS[0],
        )
        .unwrap();
        assert!(first.rollback_bundle.is_none());
        assert_eq!(
            fs::canonicalize(&first.router_path).unwrap().parent(),
            fs::canonicalize(&first.engine_path).unwrap().parent()
        );
        let root = install_dir.join(".aethyme-managed");
        fs::create_dir(root.join("versions/orphan")).unwrap();

        let second = bootstrap_install(
            &fake_payload(temp.path(), "0.3.0", true),
            &install_dir,
            &manifest_file(temp.path(), "0.3.0"),
            RELEASE_TARGETS[0],
        )
        .unwrap();
        assert!(
            second.rollback_bundle.as_ref().unwrap().ends_with(
                fs::read_link(root.join("previous"))
                    .unwrap()
                    .file_name()
                    .unwrap()
            )
        );
        assert!(!root.join("versions/orphan").exists());

        let third = bootstrap_install(
            &fake_payload(temp.path(), "0.4.0", true),
            &install_dir,
            &manifest_file(temp.path(), "0.4.0"),
            RELEASE_TARGETS[0],
        )
        .unwrap();
        let versions = fs::read_dir(root.join("versions"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(versions.len(), 2, "current plus exactly one rollback");
        assert!(
            third.active_bundle.ends_with(
                fs::read_link(root.join("current"))
                    .unwrap()
                    .file_name()
                    .unwrap()
            )
        );
        assert!(third.rollback_bundle.is_some());
    }

    #[test]
    fn failed_staged_quick_test_never_moves_the_active_pair() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("bin");
        bootstrap_install(
            &fake_payload(temp.path(), "0.2.0", true),
            &install_dir,
            &manifest_file(temp.path(), "0.2.0"),
            RELEASE_TARGETS[0],
        )
        .unwrap();
        let current = install_dir.join(".aethyme-managed/current");
        let before = fs::read_link(&current).unwrap();

        let error = bootstrap_install(
            &fake_payload(temp.path(), "0.3.0", false),
            &install_dir,
            &manifest_file(temp.path(), "0.3.0"),
            RELEASE_TARGETS[0],
        )
        .unwrap_err();

        assert!(error.contains("quick-test failed"));
        assert_eq!(fs::read_link(current).unwrap(), before);
    }

    #[test]
    fn incompatible_broker_schema_is_refused_without_migration() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("broker.db");
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);\n\
                 INSERT INTO meta (key, value) VALUES ('schema_version', '99');",
            )
            .unwrap();
        drop(connection);
        let manifest = parse_valid_manifest(&manifest("stable", "0.3.0")).unwrap();

        let error = inspect_broker_schema_at(&database, &manifest.compatibility).unwrap_err();

        assert!(error.contains("schema 99 is incompatible"));
    }
}
