use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn current_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        other => panic!("unsupported test platform {other:?}"),
    }
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}

fn write_manifest(root: &Path, channel: &str, version: &str, preview: bool) {
    let directory = if preview {
        root.join("releases/download/preview")
    } else {
        root.join("releases/latest/download")
    };
    fs::create_dir_all(&directory).unwrap();
    let targets = [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
    ];
    let artifacts = targets
        .iter()
        .map(|target| {
            json!({
                "archive": format!("aethyme-v{version}-{target}.tar.gz"),
                "binaries": ["aethyme", "aethyme-engine-cli"],
                "sha256": "b".repeat(64),
                "size_bytes": 123,
                "target": target,
            })
        })
        .collect::<Vec<_>>();
    let manifest = json!({
        "artifacts": artifacts,
        "compatibility": {
            "broker_storage": {"current_schema": 7, "minimum_readable_schema": 1},
            "engine_protocol": 1,
            "minimum_git_version": "2.38"
        },
        "installer": {
            "filename": "install.sh",
            "sha256": "c".repeat(64),
            "size_bytes": 42
        },
        "release_channel": channel,
        "required_binaries": ["aethyme", "aethyme-engine-cli"],
        "schema_version": 1,
        "source_sha": "a".repeat(40),
        "supported_platforms": targets,
        "version": version
    });
    let encoded = serde_json::to_vec_pretty(&manifest).unwrap();
    fs::write(directory.join("release-manifest.json"), &encoded).unwrap();
    let exact = root.join(format!("releases/download/v{version}"));
    fs::create_dir_all(&exact).unwrap();
    fs::write(exact.join("release-manifest.json"), encoded).unwrap();
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aethyme"));
    command.env(
        "AETHYME_RELEASE_BASE_URL",
        format!("file://{}", root.display()),
    );
    // Manifest fetches are cached host-wide. Anchoring the cache inside the
    // fixture keeps each test asking its own fake release rather than reading
    // whatever the developer's last real `update check` left on disk.
    command.env("AETHYME_HOST_CACHE_DIR", root.join("host-cache"));
    command
}

/// A release archive whose engine reports `engine_version`, so a mismatched
/// pair can be published under `version`.
fn write_fake_pair_archive(
    root: &Path,
    version: &str,
    engine_version: &str,
) -> (String, String, u64) {
    let payload = root.join("new-payload");
    fs::create_dir_all(&payload).unwrap();
    fs::write(
        payload.join("aethyme"),
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'aethyme {version}'; exit 0; fi\nif [ \"$1\" = broker ] && [ \"$2\" = quick-test ]; then echo 'broker quick test passed'; exit 0; fi\nexit 2\n"
        ),
    )
    .unwrap();
    fs::write(
        payload.join("aethyme-engine-cli"),
        format!("#!/bin/sh\necho 'aethyme-engine-cli {engine_version}'\n"),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    for binary in ["aethyme", "aethyme-engine-cli"] {
        fs::set_permissions(payload.join(binary), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let archive = format!("aethyme-v{version}-{}.tar.gz", current_target());
    let directory = root.join(format!("releases/download/v{version}"));
    fs::create_dir_all(&directory).unwrap();
    let archive_path = directory.join(&archive);
    let status = Command::new("tar")
        .args(["-czf"])
        .arg(&archive_path)
        .arg("-C")
        .arg(&payload)
        .args(["aethyme", "aethyme-engine-cli"])
        .status()
        .unwrap();
    assert!(status.success());
    let size = fs::metadata(&archive_path).unwrap().len();
    (archive, sha256(&archive_path), size)
}

fn write_executable_manifest(root: &Path, version: &str) {
    write_executable_pair_manifest(root, version, version);
}

fn write_executable_pair_manifest(root: &Path, version: &str, engine_version: &str) {
    let (archive, digest, size) = write_fake_pair_archive(root, version, engine_version);
    let targets = [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
    ];
    let artifacts = targets
        .iter()
        .map(|target| {
            let selected = *target == current_target();
            json!({
                "archive": if selected { archive.clone() } else { format!("aethyme-v{version}-{target}.tar.gz") },
                "binaries": ["aethyme", "aethyme-engine-cli"],
                "sha256": if selected { digest.clone() } else { "b".repeat(64) },
                "size_bytes": if selected { size } else { 123 },
                "target": target,
            })
        })
        .collect::<Vec<_>>();
    let manifest = json!({
        "artifacts": artifacts,
        "compatibility": {
            "broker_storage": {"current_schema": 7, "minimum_readable_schema": 1},
            "engine_protocol": 9,
            "minimum_git_version": "2.38"
        },
        "installer": {"filename": "install.sh", "sha256": "c".repeat(64), "size_bytes": 42},
        "release_channel": "stable",
        "required_binaries": ["aethyme", "aethyme-engine-cli"],
        "schema_version": 1,
        "source_sha": "a".repeat(40),
        "supported_platforms": targets,
        "version": version
    });
    let latest = root.join("releases/latest/download");
    fs::create_dir_all(&latest).unwrap();
    let encoded = serde_json::to_vec_pretty(&manifest).unwrap();
    fs::write(latest.join("release-manifest.json"), &encoded).unwrap();
    fs::write(
        root.join(format!(
            "releases/download/v{version}/release-manifest.json"
        )),
        encoded,
    )
    .unwrap();
}

fn install_managed_current(root: &Path) -> std::path::PathBuf {
    let install_dir = root.join("install/bin");
    let managed = install_dir.join(".aethyme-managed");
    let bundle = managed.join("versions/v0.2.0-current");
    fs::create_dir_all(&bundle).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_aethyme"), bundle.join("aethyme")).unwrap();
    fs::copy(
        aethyme_testkit::bins::engine_bin(),
        bundle.join("aethyme-engine-cli"),
    )
    .unwrap();
    std::os::unix::fs::symlink("versions/v0.2.0-current", managed.join("current")).unwrap();
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
    let receipt = json!({
        "schema_version": 1,
        "method": "aethyme-installer",
        "install_dir": install_dir,
        "managed_root": managed,
        "router_path": install_dir.join("aethyme"),
        "engine_path": install_dir.join("aethyme-engine-cli"),
        "current_link": managed.join("current"),
        "previous_link": managed.join("previous"),
        "versions_dir": managed.join("versions")
    });
    fs::write(
        managed.join("install-receipt.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
    install_dir.join("aethyme")
}

/// A command for the installed router, with a PATH that holds only the
/// system tools and `extra_bin` -- so the developer's own cosign (or its
/// absence) never decides what a test exercises.
fn managed(root: &Path, installed_router: &Path, extra_bin: Option<&Path>) -> Command {
    let mut command = Command::new(installed_router);
    let path = match extra_bin {
        Some(bin) => format!("{}:/usr/bin:/bin", bin.display()),
        None => "/usr/bin:/bin".to_string(),
    };
    command
        .env(
            "AETHYME_RELEASE_BASE_URL",
            format!("file://{}", root.display()),
        )
        .env("AETHYME_HOST_CACHE_DIR", root.join("host-cache"))
        .env("PATH", path)
        .current_dir(root);
    command
}

/// A `cosign` that exits `status` and records its arguments.
fn fake_cosign(root: &Path, status: i32) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = root.join("fake-bin");
    fs::create_dir_all(&bin).unwrap();
    let cosign = bin.join("cosign");
    fs::write(
        &cosign,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit {status}\n",
            root.join("cosign-args").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&cosign, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Publish a signature bundle beside the exact release manifest.
fn write_signature_bundle(root: &Path, version: &str) {
    fs::write(
        root.join(format!(
            "releases/download/v{version}/release-manifest.sigstore.json"
        )),
        "{}",
    )
    .unwrap();
}

fn plan_managed_update(root: &Path, installed_router: &Path) -> Value {
    let output = managed(root, installed_router, None)
        .args(["update", "plan", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn update_help_is_explicit_and_never_background() {
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(["update", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    // Requested help is output: stdout, exit 0 (Phase 4, P4.3).
    let stderr = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "update check",
        "update plan [--channel stable|preview] [--version X.Y.Z] [--json] [--refresh]",
        "update execute --confirm <manifest-sha256>",
        "update apply",
        "self-update",
        "--require-signature",
        "--no-verify-signature",
        "never runs in the background",
        "brew upgrade aethyme",
        "aethyme upgrade plan",
        // The cache changes what a check costs and what it can be trusted to
        // mean, so the help has to say it is there and how to turn it off.
        "--refresh bypasses the cache",
        "AETHYME_UPDATE_CACHE_TTL_SECONDS=0",
        "the plan is recomputed every run",
    ] {
        assert!(stderr.contains(expected), "missing {expected:?}\n{stderr}");
    }
}

#[test]
fn stable_plan_returns_manifest_bound_json_without_mutating_manual_installs() {
    let temp = tempfile::tempdir().unwrap();
    write_manifest(temp.path(), "stable", "9.0.0", false);

    let output = command(temp.path())
        .args(["update", "plan", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["schema_version"], 1);
    assert_eq!(plan["channel"], "stable");
    assert_eq!(plan["target_version"], "9.0.0");
    assert_eq!(plan["manifest_sha256"].as_str().unwrap().len(), 64);
    assert!(matches!(
        plan["installation"]["method"].as_str(),
        Some("manual_archive" | "unknown")
    ));
    assert_eq!(plan["action"], "adopt_installer");
}

#[test]
fn preview_plan_uses_only_the_explicit_preview_channel() {
    let temp = tempfile::tempdir().unwrap();
    write_manifest(temp.path(), "preview", "9.1.0", true);

    let output = command(temp.path())
        .args(["update", "plan", "--channel", "preview", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let plan: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["channel"], "preview");
    assert!(
        plan["manifest_url"]
            .as_str()
            .unwrap()
            .ends_with("/releases/download/v9.1.0/release-manifest.json")
    );
}

#[test]
fn check_is_stable_and_channel_mismatches_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    write_manifest(temp.path(), "preview", "9.0.0", false);

    let output = command(temp.path())
        .args(["update", "check", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not requested stable"));
}

#[test]
fn confirmed_execute_switches_the_pair_and_retains_the_previous_bundle() {
    let temp = tempfile::tempdir().unwrap();
    write_executable_manifest(temp.path(), "9.0.0");
    let installed_router = install_managed_current(temp.path());

    let plan = plan_managed_update(temp.path(), &installed_router);
    assert_eq!(plan["action"], "execute_installer_update");
    let confirmation = plan["manifest_sha256"].as_str().unwrap();

    let execute = managed(temp.path(), &installed_router, None)
        .args(["update", "execute", "--confirm", confirmation, "--json"])
        .output()
        .unwrap();
    assert!(
        execute.status.success(),
        "{}",
        String::from_utf8_lossy(&execute.stderr)
    );
    let report: Value = serde_json::from_slice(&execute.stdout).unwrap();
    assert_eq!(report["installed_version"], "9.0.0");
    assert_eq!(report["quick_test_passed"], true);
    assert_eq!(report["signature_verification"], "skipped_cosign_missing");

    let managed = temp.path().join("install/bin/.aethyme-managed");
    assert!(
        fs::read_link(managed.join("current"))
            .unwrap()
            .to_string_lossy()
            .starts_with("versions/v9.0.0-")
    );
    assert_eq!(
        fs::read_link(managed.join("previous")).unwrap(),
        Path::new("versions/v0.2.0-current")
    );
    let version = Command::new(&installed_router)
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().trim(),
        "aethyme 9.0.0"
    );
    assert!(
        !managed
            .join("update-plans")
            .join(format!("{confirmation}.json"))
            .exists()
    );
}

#[test]
fn checksum_mismatch_is_refused_without_moving_the_active_pair() {
    let temp = tempfile::tempdir().unwrap();
    write_executable_manifest(temp.path(), "9.0.0");
    let installed_router = install_managed_current(temp.path());
    let plan = plan_managed_update(temp.path(), &installed_router);
    let confirmation = plan["manifest_sha256"].as_str().unwrap();
    let archive_url = plan["archive"]["url"].as_str().unwrap();
    let archive = Path::new(archive_url.strip_prefix("file://").unwrap());
    let mut bytes = fs::read(archive).unwrap();
    *bytes.last_mut().unwrap() ^= 0xff;
    fs::write(archive, bytes).unwrap();
    let current = temp.path().join("install/bin/.aethyme-managed/current");
    let before = fs::read_link(&current).unwrap();

    let execute = managed(temp.path(), &installed_router, None)
        .args(["update", "execute", "--confirm", confirmation])
        .output()
        .unwrap();

    assert!(!execute.status.success());
    assert!(String::from_utf8_lossy(&execute.stderr).contains("SHA-256 mismatch"));
    assert_eq!(fs::read_link(current).unwrap(), before);
}

fn current_link(root: &Path) -> std::path::PathBuf {
    fs::read_link(root.join("install/bin/.aethyme-managed/current")).unwrap()
}

#[test]
fn a_failed_manifest_signature_is_refused_without_moving_the_active_pair() {
    let temp = tempfile::tempdir().unwrap();
    write_executable_manifest(temp.path(), "9.0.0");
    write_signature_bundle(temp.path(), "9.0.0");
    let installed_router = install_managed_current(temp.path());
    let plan = plan_managed_update(temp.path(), &installed_router);
    let confirmation = plan["manifest_sha256"].as_str().unwrap();
    let before = current_link(temp.path());
    let cosign = fake_cosign(temp.path(), 1);

    let execute = managed(temp.path(), &installed_router, Some(&cosign))
        .args(["update", "execute", "--confirm", confirmation])
        .output()
        .unwrap();

    assert!(!execute.status.success());
    assert!(
        String::from_utf8_lossy(&execute.stderr).contains("signature verification failed"),
        "{}",
        String::from_utf8_lossy(&execute.stderr)
    );
    assert_eq!(current_link(temp.path()), before);
}

#[test]
fn self_update_plans_verifies_the_signature_and_switches_the_pair() {
    let temp = tempfile::tempdir().unwrap();
    write_executable_manifest(temp.path(), "9.0.0");
    write_signature_bundle(temp.path(), "9.0.0");
    let installed_router = install_managed_current(temp.path());
    let cosign = fake_cosign(temp.path(), 0);

    let output = managed(temp.path(), &installed_router, Some(&cosign))
        .args(["self-update", "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["installed_version"], "9.0.0");
    assert_eq!(report["signature_verification"], "verified");
    let arguments = fs::read_to_string(temp.path().join("cosign-args")).unwrap();
    assert!(
        arguments.contains(
            "https://github.com/schiste/Aethyme/.github/workflows/release.yml@refs/tags/v9.0.0"
        ),
        "{arguments}"
    );
    assert!(arguments.contains("release-manifest.json"), "{arguments}");
    assert!(
        current_link(temp.path())
            .to_string_lossy()
            .starts_with("versions/v9.0.0-")
    );
}

#[test]
fn require_signature_without_cosign_refuses_before_installing() {
    let temp = tempfile::tempdir().unwrap();
    write_executable_manifest(temp.path(), "9.0.0");
    let installed_router = install_managed_current(temp.path());
    let before = current_link(temp.path());

    let output = managed(temp.path(), &installed_router, None)
        .args(["update", "apply", "--require-signature"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--require-signature needs cosign"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(current_link(temp.path()), before);
}

#[test]
fn a_mismatched_pair_is_refused_without_moving_the_active_pair() {
    let temp = tempfile::tempdir().unwrap();
    write_executable_pair_manifest(temp.path(), "9.0.0", "8.9.0");
    let installed_router = install_managed_current(temp.path());
    let before = current_link(temp.path());

    let output = managed(temp.path(), &installed_router, None)
        .args(["update", "apply"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("aethyme-engine-cli"), "{stderr}");
    // Refused while staging, before anything was switched -- not caught only
    // by the post-activation check that rolls back.
    assert!(!stderr.contains("rolled back"), "{stderr}");
    assert_eq!(current_link(temp.path()), before);
}

#[test]
fn a_pinned_version_plans_that_exact_release() {
    let temp = tempfile::tempdir().unwrap();
    write_executable_manifest(temp.path(), "9.0.0");
    let installed_router = install_managed_current(temp.path());
    fs::remove_dir_all(temp.path().join("releases/latest")).unwrap();

    let pinned = managed(temp.path(), &installed_router, None)
        .args(["update", "plan", "--version", "v9.0.0", "--json"])
        .output()
        .unwrap();
    assert!(
        pinned.status.success(),
        "{}",
        String::from_utf8_lossy(&pinned.stderr)
    );
    let plan: Value = serde_json::from_slice(&pinned.stdout).unwrap();
    assert_eq!(plan["target_version"], "9.0.0");

    let missing = managed(temp.path(), &installed_router, None)
        .args(["update", "plan", "--version", "9.9.9"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
}
